using UnrealBuildTool;
public class EchoAvatarEditorTarget : TargetRules {
 public EchoAvatarEditorTarget(TargetInfo Target) : base(Target) {
  Type=TargetType.Editor; DefaultBuildSettings=BuildSettingsVersion.V7;
  IncludeOrderVersion=EngineIncludeOrderVersion.Unreal5_8;
  ExtraModuleNames.Add("EchoAvatar");
 }
}