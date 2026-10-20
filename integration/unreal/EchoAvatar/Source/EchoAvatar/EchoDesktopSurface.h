#pragma once
#include "CoreMinimal.h"
#include "Windows/AllowWindowsPlatformTypes.h"
#include <Windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <dcomp.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include "Windows/HideWindowsPlatformTypes.h"
// GPU-only composition surface; does not modify the engine's game swap chain.
class FEchoDesktopSurface {
 struct FDpiScope { DPI_AWARENESS_CONTEXT Previous;
  FDpiScope():Previous(SetThreadDpiAwarenessContext(reinterpret_cast<DPI_AWARENESS_CONTEXT>(-4))) {}
  ~FDpiScope() { if(Previous) SetThreadDpiAwarenessContext(Previous); }
 };
 static LRESULT CALLBACK WindowProc(HWND Window,UINT Message,WPARAM WParam,LPARAM LParam) {
  if(Message==WM_NCHITTEST) return HTTRANSPARENT;
  return DefWindowProcW(Window,Message,WParam,LParam);
 }

 template<class T> using Ptr=Microsoft::WRL::ComPtr<T>;
 HWND Window=nullptr; Ptr<ID3D11Device> Device; Ptr<ID3D11DeviceContext> Context;
 Ptr<IDXGISwapChain1> Swap; Ptr<IDCompositionDevice> Composition; Ptr<IDCompositionTarget> Target;
 Ptr<IDCompositionVisual> Visual; Ptr<ID3D11RenderTargetView> Output;
 Ptr<ID3D11VertexShader> Vertex; Ptr<ID3D11PixelShader> Pixel; Ptr<ID3D11SamplerState> Sampler;
 Ptr<ID3D11Texture2D> ProbeTexture;
 Ptr<ID3D11Texture2D> Shared; Ptr<ID3D11ShaderResourceView> Input; HANDLE LastHandle=nullptr;
public:
 ~FEchoDesktopSurface() { if(Window) DestroyWindow(Window); }
 bool Initialize() {
  FDpiScope DpiScope;
  WNDCLASSW Class{}; Class.lpfnWndProc=WindowProc; Class.hInstance=GetModuleHandleW(nullptr); Class.lpszClassName=L"EchoAvatarComposition";
  RegisterClassW(&Class);
  Window=CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP|WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE|WS_EX_TRANSPARENT,
   Class.lpszClassName,L"Echo 3D character",WS_POPUP,0,0,180,360,nullptr,nullptr,Class.hInstance,nullptr);
  if(!Window) return false;
  if(FAILED(D3D11CreateDevice(nullptr,D3D_DRIVER_TYPE_HARDWARE,nullptr,D3D11_CREATE_DEVICE_BGRA_SUPPORT,nullptr,0,D3D11_SDK_VERSION,&Device,nullptr,&Context))) return false;
  Ptr<IDXGIDevice> DxgiDevice; Device.As(&DxgiDevice); Ptr<IDXGIAdapter> Adapter; DxgiDevice->GetAdapter(&Adapter);
  Ptr<IDXGIFactory2> Factory; Adapter->GetParent(IID_PPV_ARGS(&Factory));
  DXGI_SWAP_CHAIN_DESC1 Desc{}; Desc.Width=360; Desc.Height=720; Desc.Format=DXGI_FORMAT_B8G8R8A8_UNORM;
  Desc.SampleDesc.Count=1; Desc.BufferUsage=DXGI_USAGE_RENDER_TARGET_OUTPUT; Desc.BufferCount=2;
  Desc.SwapEffect=DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL; Desc.AlphaMode=DXGI_ALPHA_MODE_PREMULTIPLIED;
  if(FAILED(Factory->CreateSwapChainForComposition(Device.Get(),&Desc,nullptr,&Swap))) return false;
  if(FAILED(DCompositionCreateDevice(DxgiDevice.Get(),IID_PPV_ARGS(&Composition)))) return false;
  if(FAILED(Composition->CreateTargetForHwnd(Window,1,&Target))||FAILED(Composition->CreateVisual(&Visual))) return false;
  Visual->SetContent(Swap.Get()); Target->SetRoot(Visual.Get()); Composition->Commit();
  Ptr<ID3D11Texture2D> Buffer; Swap->GetBuffer(0,IID_PPV_ARGS(&Buffer)); Device->CreateRenderTargetView(Buffer.Get(),nullptr,&Output);
  const char* Shader=R"(
   Texture2D image:register(t0); SamplerState sampleImage:register(s0);
   struct V { float4 pos:SV_POSITION; float2 uv:TEXCOORD0; };
   V VS(uint id:SV_VertexID) { V o; o.uv=float2((id<<1)&2,id&2); o.pos=float4(o.uv*float2(2,-2)+float2(-1,1),0,1); return o; }
   float4 PS(V i):SV_TARGET { float4 c=image.Sample(sampleImage,i.uv); float a=saturate(1-c.a); return float4(pow(max(c.rgb,0),1/2.2)*a,a); }
  )";
  Ptr<ID3DBlob> Vs,Ps,Error;
  if(FAILED(D3DCompile(Shader,strlen(Shader),nullptr,nullptr,nullptr,"VS","vs_5_0",0,0,&Vs,&Error))) return false;
  if(FAILED(D3DCompile(Shader,strlen(Shader),nullptr,nullptr,nullptr,"PS","ps_5_0",0,0,&Ps,&Error))) return false;
  Device->CreateVertexShader(Vs->GetBufferPointer(),Vs->GetBufferSize(),nullptr,&Vertex);
  Device->CreatePixelShader(Ps->GetBufferPointer(),Ps->GetBufferSize(),nullptr,&Pixel);
  D3D11_SAMPLER_DESC Sampling{}; Sampling.Filter=D3D11_FILTER_MIN_MAG_MIP_LINEAR; Sampling.AddressU=Sampling.AddressV=Sampling.AddressW=D3D11_TEXTURE_ADDRESS_CLAMP;
  Sampling.MaxLOD=D3D11_FLOAT32_MAX; Device->CreateSamplerState(&Sampling,&Sampler);
  return Vertex&&Pixel&&Sampler&&Output;
 }
 bool Probe(int X,int Y,bool Visible) {
  if(!ProbeTexture) {
   // A synthetic colored tile verifies composition without creating a model.
   TArray<uint32> Pixels; Pixels.SetNum(360*720);
   for(int y=0;y<720;y++) for(int x=0;x<360;x++) Pixels[y*360+x]=(x>90&&x<270&&y>80&&y<600)?0x000090D0:0xff000000;
   D3D11_TEXTURE2D_DESC Desc{}; Desc.Width=360; Desc.Height=720; Desc.MipLevels=1; Desc.ArraySize=1;
   Desc.Format=DXGI_FORMAT_B8G8R8A8_UNORM; Desc.SampleDesc.Count=1; Desc.BindFlags=D3D11_BIND_SHADER_RESOURCE; Desc.MiscFlags=D3D11_RESOURCE_MISC_SHARED;
   D3D11_SUBRESOURCE_DATA Data{Pixels.GetData(),360*4,0};
   if(FAILED(Device->CreateTexture2D(&Desc,&Data,&ProbeTexture))) return false;
  }
  return Present(ProbeTexture.Get(),X,Y,1.f,Visible);
 }
 bool Present(ID3D11Resource* Resource,int X,int Y,float Scale,bool Visible) {
  FDpiScope DpiScope;
  if(!Window||!Resource) return false;
  Ptr<IDXGIResource> Dxgi; if(FAILED(Resource->QueryInterface(IID_PPV_ARGS(&Dxgi)))) return false;
  HANDLE Handle=nullptr; if(FAILED(Dxgi->GetSharedHandle(&Handle))) return false;
  if(Handle!=LastHandle) {
   Input.Reset(); Shared.Reset();
   if(FAILED(Device->OpenSharedResource(Handle,IID_PPV_ARGS(&Shared)))) return false;
   if(FAILED(Device->CreateShaderResourceView(Shared.Get(),nullptr,&Input))) return false;
   LastHandle=Handle;
  }
  D3D11_VIEWPORT Viewport{0,0,360,720,0,1}; Context->RSSetViewports(1,&Viewport);
  auto* View=Output.Get(); Context->OMSetRenderTargets(1,&View,nullptr);
  const float Clear[4]={0,0,0,0}; Context->ClearRenderTargetView(View,Clear);
  Context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  Context->VSSetShader(Vertex.Get(),nullptr,0); Context->PSSetShader(Pixel.Get(),nullptr,0);
  auto* InputView=Input.Get(); Context->PSSetShaderResources(0,1,&InputView); auto* Sampling=Sampler.Get(); Context->PSSetSamplers(0,1,&Sampling);
  Context->Draw(3,0); auto Result=Swap->Present(1,0);
  // GPU render size is fixed; the visual scales to the user-defined character size.
  const float Dpi=(float)GetDpiForWindow(Window)/96.f; Scale*=Dpi;
  Ptr<IDCompositionScaleTransform> Transform; Composition->CreateScaleTransform(&Transform);
  Transform->SetScaleX(0.5f*Scale); Transform->SetScaleY(0.5f*Scale); Visual->SetTransform(Transform.Get()); Composition->Commit();
  SetWindowPos(Window,HWND_TOPMOST,X,Y,(int)(180*Scale),(int)(360*Scale),SWP_NOACTIVATE|(Visible?SWP_SHOWWINDOW:SWP_HIDEWINDOW));
  return SUCCEEDED(Result);
 }
};
