; BuildCast Windows installer
; build: ISCC buildcast.iss   (expects release binaries in ..\..\target\release)

#define AppName "BuildCast"
#define AppPublisher "NetTimeLogic GmbH"
#define TargetDir "..\..\target\release"
; version comes from the exe resource, which winresource fills from the
; workspace Cargo.toml — no second place to bump
#define FullVer GetVersionNumbersString(TargetDir + "\buildcast.exe")
#define AppVersion Copy(FullVer, 1, RPos(".", FullVer) - 1)

[Setup]
AppId={{7B7C63D4-1F2E-4A5B-9C0D-BC0DCA57B1D1}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\gui\assets\buildcast.ico
UninstallDisplayIcon={app}\buildcast.exe
OutputDir=Output
OutputBaseFilename=buildcast-setup-{#AppVersion}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
; offers "all users" (Program Files) or "me only" (%LocalAppData%\Programs)
PrivilegesRequiredOverridesAllowed=dialog commandline
DisableProgramGroupPage=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "addtopath"; Description: "Add the CLI (buildcast-cli) to the system &PATH"; GroupDescription: "Command line:"

[Files]
Source: "{#TargetDir}\buildcast.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#TargetDir}\buildcast-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\buildcast.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\buildcast.exe"; Tasks: desktopicon

[Registry]
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; \
    ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
    Tasks: addtopath; Check: IsAdminInstallMode and NeedsAddPath('{app}')
Root: HKCU; Subkey: "Environment"; \
    ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
    Tasks: addtopath; Check: (not IsAdminInstallMode) and NeedsAddPath('{app}')

[Run]
; shellexec: the exe's requireAdministrator manifest needs ShellExecute (UAC);
; plain CreateProcess from the de-elevated postinstall context fails with 740
Filename: "{app}\buildcast.exe"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent shellexec

[Code]
function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
  AppDir: string;
  RootKey: Integer;
  SubKey: string;
begin
  AppDir := ExpandConstant(Param);
  if IsAdminInstallMode then
  begin
    RootKey := HKLM;
    SubKey := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  end
  else
  begin
    RootKey := HKCU;
    SubKey := 'Environment';
  end;
  if not RegQueryStringValue(RootKey, SubKey, 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(AppDir) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;
