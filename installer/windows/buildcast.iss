; BuildCast Windows installer
; build: ISCC buildcast.iss   (expects release binaries in ..\..\target\release)

#define AppName "BuildCast"
#define AppVersion "0.1.0"
#define AppPublisher "NetTimeLogic GmbH"
#define TargetDir "..\..\target\release"

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
    Tasks: addtopath; Check: NeedsAddPath('{app}')

[Run]
Filename: "{app}\buildcast.exe"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
  AppDir: string;
begin
  AppDir := ExpandConstant(Param);
  if not RegQueryStringValue(HKLM,
    'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(AppDir) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;
