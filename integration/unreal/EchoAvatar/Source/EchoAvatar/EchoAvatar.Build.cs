using UnrealBuildTool;
public class EchoAvatar : ModuleRules {
 public EchoAvatar(ReadOnlyTargetRules Target) : base(Target) {
  PCHUsage=PCHUsageMode.UseExplicitOrSharedPCHs;
  PublicDependencyModuleNames.AddRange(new[]{"Core","CoreUObject","Engine","LiveLinkInterface","MetaHumanLocalLiveLinkSource","MetaHumanPipelineCore","SpeechAnimationSolver","MetaHumanCoreTech"});
  PrivateDependencyModuleNames.AddRange(new[]{"Json","JsonUtilities","Projects","Slate","SlateCore","RenderCore","RHI","PakFile"});
  if(Target.Platform==UnrealTargetPlatform.Win64) { PublicSystemLibraries.AddRange(new[]{"d3d11.lib","dxgi.lib","dcomp.lib","d3dcompiler.lib"}); }
 }
}
