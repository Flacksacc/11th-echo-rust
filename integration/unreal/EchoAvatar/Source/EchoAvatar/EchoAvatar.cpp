#include "CoreMinimal.h"
#include "Modules/ModuleManager.h"
#include "Containers/Ticker.h"
#include "Features/IModularFeatures.h"
#include "ILiveLinkClient.h"
#include "EchoAudioSource.h"
#include "EchoDesktopSurface.h"
#include "TextureResource.h"
#include "EchoAvatarComponent.h"
#include "Engine/Engine.h"
#include "Engine/World.h"
#include "Engine/TextureRenderTarget2D.h"
#include "Components/SceneCaptureComponent2D.h"
#include "GameFramework/Actor.h"
#include "Engine/DirectionalLight.h"
#include "Components/LightComponent.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"
#include "Async/Async.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "HAL/PlatformFileManager.h"
#include "IPlatformFilePak.h"
#include "Framework/Application/SlateApplication.h"
#include "Engine/GameViewportClient.h"
#include "Widgets/SWindow.h"
#include "Windows/AllowWindowsPlatformTypes.h"
#include <Windows.h>
#include "Windows/HideWindowsPlatformTypes.h"

class FEchoAvatarModule final : public FDefaultGameModuleImpl {
 TSharedPtr<FEchoAudioSource> Audio;
 TQueue<TSharedPtr<FJsonObject>,EQueueMode::Spsc> Incoming;
 FThreadSafeCounter Pending;
 TAtomic<bool> Stopping{false}; TFuture<void> Reader;
 HANDLE Pipe=INVALID_HANDLE_VALUE;
 FTSTicker::FDelegateHandle TickHandle;
 TAtomic<bool> Connected{false}; bool Rendered=false,SentReady=false,LastReady=false; int LastFacialFrames=-1;
 TArray<TSharedPtr<FJsonObject>> AudioQueue;
 uint64 Generation=0,ClockMicros=0; bool HadState=false;
 TWeakObjectPtr<AActor> Character;
 TStrongObjectPtr<UTextureRenderTarget2D> RenderTarget;
 TWeakObjectPtr<USceneCaptureComponent2D> Capture;
 TUniquePtr<FEchoDesktopSurface> Surface;
 int OverlayX=80,OverlayY=80; float CharacterScale=1; bool Visible=false;
 double LastFrame=0;

 FString Blueprint=TEXT("/Game/EchoAvatar/BP_EchoAvatar.BP_EchoAvatar_C");
 void ReadLoop(FString Name) {
  while(!Stopping) {
   Pipe=CreateFileW(*Name,GENERIC_READ|GENERIC_WRITE,0,nullptr,OPEN_EXISTING,0,nullptr);
   if(Pipe!=INVALID_HANDLE_VALUE) break;
   FPlatformProcess::Sleep(0.1f);
  }
  if(Stopping) return;
  const ANSICHAR* Hello="{\"version\":1,\"type\":\"hello\"}";
  DWORD Length=(DWORD)FCStringAnsi::Strlen(Hello),Written=0;
  if(!WriteFile(Pipe,&Length,4,&Written,nullptr)||Written!=4||!WriteFile(Pipe,Hello,Length,&Written,nullptr)||Written!=Length) return;
  Connected=true;
  while(!Stopping) {
   uint32 Size=0; if(!ReadExact(&Size,4)||Size>256*1024) break;
   TArray<uint8> Bytes; Bytes.SetNumUninitialized(Size+1);
   if(!ReadExact(Bytes.GetData(),Size)) break; Bytes[Size]=0;
   TSharedPtr<FJsonObject> Object;
   if(!FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(UTF8_TO_TCHAR((const char*)Bytes.GetData())),Object)) break;
   if(Object->GetIntegerField(TEXT("version"))!=1) break;
   // Drop excess animation data without growing memory or slowing Echo audio.
   if(Pending.GetValue()<24) { Incoming.Enqueue(Object); Pending.Increment(); }
  }
 }
 bool ReadExact(void* Data,uint32 Size) {
  uint32 Offset=0;
  while(Offset<Size&&!Stopping) { DWORD Available=0; if(!PeekNamedPipe(Pipe,nullptr,0,nullptr,&Available,nullptr)) return false; if(!Available) { FPlatformProcess::Sleep(0.01f); continue; } DWORD Count=0; if(!ReadFile(Pipe,(uint8*)Data+Offset,FMath::Min(Size-Offset,(uint32)Available),&Count,nullptr)||!Count) return false; Offset+=Count; }
  return Offset==Size;
 }
 bool Tick(float Delta) {
  if(FParse::Param(FCommandLine::Get(),TEXT("EchoSurfaceProbe"))) { if(!Surface) { Surface=MakeUnique<FEchoDesktopSurface>(); if(!Surface->Initialize()) Surface.Reset(); } if(Surface) Rendered=Surface->Probe(OverlayX,OverlayY,Visible); }
  if(HadState&&!Audio&&IModularFeatures::Get().IsModularFeatureAvailable(ILiveLinkClient::ModularFeatureName)) {
   Audio=MakeShared<FEchoAudioSource>();
   IModularFeatures::Get().GetModularFeature<ILiveLinkClient>(ILiveLinkClient::ModularFeatureName).AddSource(Audio);
  }
  if(Audio&&!Audio->Subject&&Audio->Client) { Audio->Reset(); }
  // The companion's normal game surface must never appear as a desktop panel.
  if(GEngine&&GEngine->GameViewport) {
   auto Window=GEngine->GameViewport->GetWindow(); if(Window.IsValid()) Window->HideWindow();
  }
  TSharedPtr<FJsonObject> Object;
  while(Incoming.Dequeue(Object)) {
   Pending.Decrement(); const FString Type=Object->GetStringField(TEXT("type"));
   if(Type==TEXT("clock")) { if((uint64)Object->GetNumberField(TEXT("generation"))==Generation) ClockMicros=(uint64)Object->GetNumberField(TEXT("micros")); }
   if(Type==TEXT("state")) {
    const auto Data=Object->GetObjectField(TEXT("data"));
    const uint64 Next=(uint64)Data->GetNumberField(TEXT("generation"));
    if(Next!=Generation) { Generation=Next; ClockMicros=0; AudioQueue.Reset(); if(Audio) Audio->Reset(); }
    HadState=true;
    const auto AvatarSettings=Data->GetObjectField(TEXT("avatar"));
    OverlayX=AvatarSettings->GetIntegerField(TEXT("x")); OverlayY=AvatarSettings->GetIntegerField(TEXT("y"));
    CharacterScale=(float)AvatarSettings->GetNumberField(TEXT("scale")); Visible=AvatarSettings->GetBoolField(TEXT("visible"));
    if(!Character.IsValid()&&GEngine) {
     for(const auto& Context:GEngine->GetWorldContexts()) { if(Context.World()&&Context.World()->IsGameWorld()) {
      if(UClass* Class=LoadClass<AActor>(nullptr,*Blueprint)) {
       Character=Context.World()->SpawnActor<AActor>(Class);
       RenderTarget.Reset(NewObject<UTextureRenderTarget2D>());
       RenderTarget->RenderTargetFormat=RTF_RGBA16f; RenderTarget->bGPUSharedFlag=true; RenderTarget->ClearColor=FLinearColor(0,0,0,1);
       RenderTarget->InitAutoFormat(360,720); RenderTarget->UpdateResourceImmediate();
       auto* Camera=NewObject<USceneCaptureComponent2D>(Character.Get()); Camera->RegisterComponent();
       Camera->TextureTarget=RenderTarget.Get(); Camera->CaptureSource=SCS_SceneColorHDR; Camera->ProjectionType=ECameraProjectionMode::Orthographic;
       const float Height=FMath::Max(100.f,Character->GetComponentsBoundingBox().GetSize().Z);
       Camera->OrthoWidth=Height*180.f/292.f; Camera->SetWorldLocation(FVector(450,0,Height*(1.f-360.f/584.f)));
       auto* Light=Context.World()->SpawnActor<ADirectionalLight>(); Light->SetActorRotation(FRotator(-25,-25,0)); Light->GetLightComponent()->SetIntensity(4.f); Camera->SetWorldRotation(FRotator(0,180,0));
       Camera->PrimitiveRenderMode=ESceneCapturePrimitiveRenderMode::PRM_UseShowOnlyList; Camera->ShowOnlyActorComponents(Character.Get());
       Camera->bCaptureEveryFrame=false; Camera->bCaptureOnMovement=false; Capture=Camera;
       Surface=MakeUnique<FEchoDesktopSurface>(); if(!Surface->Initialize()) Surface.Reset();
      }
      break;
     } }
    }
    if(Character.IsValid()) { if(auto* Component=Character->FindComponentByClass<UEchoAvatarComponent>()) {
     Component->AssistantState=Data->GetStringField(TEXT("state")); Component->Listening=Data->GetBoolField(TEXT("listening"));
     const auto Avatar=Data->GetObjectField(TEXT("avatar")); Component->Sitting=Avatar->GetBoolField(TEXT("sitting"));
     Component->LedgeWidth=(float)Avatar->GetNumberField(TEXT("ledge_width"));
    } }
   }
   if(Type==TEXT("audio")&&AudioQueue.Num()<16) AudioQueue.Add(Object->GetObjectField(TEXT("data")));
  }
  while(AudioQueue.Num()&&HadState&&Audio&&Audio->Subject) {
   const auto Data=AudioQueue[0];
   const uint64 Gen=(uint64)Data->GetNumberField(TEXT("generation"));
   const int32 Rate=Data->GetIntegerField(TEXT("sample_rate")); const uint64 Start=(uint64)Data->GetNumberField(TEXT("start_sample"));
   if(Rate<8000||Rate>96000||Gen!=Generation||(Start*1000000/Rate+200000)<ClockMicros) { AudioQueue.RemoveAt(0); continue; }
   if(Start*1000000/Rate>ClockMicros+80000) break;
   const auto& Values=Data->GetArrayField(TEXT("samples"));
   if(Values.Num()<=2048) { TArray<float> Samples; Samples.Reserve(Values.Num()); for(const auto& Value:Values) Samples.Add((float)Value->AsNumber()); Audio->Subject->Push(MoveTemp(Samples),Rate,Start); }
   AudioQueue.RemoveAt(0);
  }
  if(Surface&&Capture.IsValid()&&RenderTarget.IsValid()&&FPlatformTime::Seconds()-LastFrame>1.0/30.0) {
   LastFrame=FPlatformTime::Seconds(); Capture->CaptureScene(); FlushRenderingCommands();
   auto* Resource=RenderTarget->GameThread_GetRenderTargetResource()->GetRenderTargetTexture()->GetNativeResource();
   Rendered=Surface->Present(static_cast<ID3D11Resource*>(Resource),OverlayX,OverlayY,CharacterScale,Visible);
  }
  const int Frames=Audio&&Audio->Client?Audio->Client->GetSubjectFrameTimes(FLiveLinkSubjectName(TEXT("EchoSpeech"))).Num():0;
  if(Connected&&(!SentReady||Rendered!=LastReady||Frames!=LastFacialFrames)) {
   const FString Response=FString::Printf(TEXT("{\"version\":1,\"type\":\"ready\",\"rendered\":%s,\"facial_frames\":%d}"),Rendered?TEXT("true"):TEXT("false"),Frames);
   FTCHARToUTF8 Bytes(*Response); DWORD Size=(DWORD)Bytes.Length(),Count=0;
   WriteFile(Pipe,&Size,4,&Count,nullptr); WriteFile(Pipe,Bytes.Get(),Size,&Count,nullptr);
   SentReady=true; LastReady=Rendered; LastFacialFrames=Frames;
  }
  return true;
 }
public:
 void StartupModule() override {
  FString Name; if(!FParse::Value(FCommandLine::Get(),TEXT("EchoPipe="),Name)||!Name.StartsWith(LR"(\\.\pipe\EchoAvatar-)")) return;
  FString Pack;
  if(FParse::Value(FCommandLine::Get(),TEXT("EchoAvatarPack="),Pack)) {
   auto* Platform=static_cast<FPakPlatformFile*>(FPlatformFileManager::Get().FindPlatformFile(TEXT("PakFile")));
   if(!Platform||!Platform->Mount(*Pack,0)) return;
  }
  Reader=Async(EAsyncExecution::Thread,[this,Name](){ReadLoop(Name);});
  TickHandle=FTSTicker::GetCoreTicker().AddTicker(FTickerDelegate::CreateRaw(this,&FEchoAvatarModule::Tick));
 }
 void ShutdownModule() override {
  Stopping=true; FTSTicker::GetCoreTicker().RemoveTicker(TickHandle);
  if(Reader.IsValid()) Reader.Wait();
  if(Pipe!=INVALID_HANDLE_VALUE) { CloseHandle(Pipe); Pipe=INVALID_HANDLE_VALUE; }
  Surface.Reset(); RenderTarget.Reset();
  if(Audio) { Audio->RequestSourceShutdown(); Audio.Reset(); }
 }
};
IMPLEMENT_PRIMARY_GAME_MODULE(FEchoAvatarModule,EchoAvatar,"EchoAvatar");
