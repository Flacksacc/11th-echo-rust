#define AppName "Echo"
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define AppPublisher "Echo contributors"
#define AppExeName "echo.exe"

[Setup]
AppId={{BCE66B5C-943D-43D6-B3B3-1A04B7DE82AB}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={localappdata}\Programs\Echo
DefaultGroupName=Echo
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=output
OutputBaseFilename=Echo-{#AppVersion}-Setup
SetupIconFile=..\eleventhecho.ico
UninstallDisplayIcon={app}\{#AppExeName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked
Name: "startup"; Description: "Start Echo when I sign in to Windows"; GroupDescription: "Windows startup:"; Flags: checkedonce

[Files]
Source: "..\target\release\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\eleventhecho.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\eleventhecho.ico"; DestDir: "{app}"; Flags: ignoreversion
; Local speech models are deliberately absent. The application downloads and
; verifies them in the user's local app-data directory when local speech is used.
Source: "THIRD_PARTY_NOTICES.md"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "licenses\*.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\Echo"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\Echo"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Echo"; ValueData: """{app}\{#AppExeName}"" --startup"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\{#AppExeName}"; Description: "Launch Echo"; Flags: nowait postinstall skipifsilent

[InstallDelete]
Type: files; Name: "{app}\eleventh_echo_rust.exe"
Type: files; Name: "{group}\11th Echo.lnk"
Type: files; Name: "{autodesktop}\11th Echo.lnk"

[Code]
var
  HadLegacyStartup: Boolean;

function InitializeSetup(): Boolean;
begin
  HadLegacyStartup := RegValueExists(
    HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '11th Echo');
  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  StartupCommand: String;
begin
  if CurStep = ssPostInstall then
  begin
    if HadLegacyStartup and
       (not RegValueExists(HKCU,
         'Software\Microsoft\Windows\CurrentVersion\Run', 'Echo')) then
    begin
      StartupCommand := '"' + ExpandConstant('{app}\{#AppExeName}') + '" --startup';
      RegWriteStringValue(HKCU,
        'Software\Microsoft\Windows\CurrentVersion\Run', 'Echo', StartupCommand);
    end;
    RegDeleteValue(HKCU,
      'Software\Microsoft\Windows\CurrentVersion\Run', '11th Echo');
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Echo');
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '11th Echo');

    if (not UninstallSilent) and
       DirExists(ExpandConstant('{localappdata}\11th_echo\models')) and
       (MsgBox('Remove the downloaded local speech model files too?',
         mbConfirmation, MB_YESNO) = IDYES) then
      DelTree(ExpandConstant('{localappdata}\11th_echo\models'), True, True, True);
  end;
end;
