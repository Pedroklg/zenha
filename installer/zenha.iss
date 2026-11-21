#ifndef AppVersion
  #define AppVersion "0.1.18"
#endif

#define AppName "Zenha"
#define AppExeName "zenha.exe"

[Setup]
AppId={{B3F9E4A2-6C1D-4E8B-9A4F-2D7C8E1B5F03}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
DefaultDirName={localappdata}\Programs\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=Zenha-{#AppVersion}-windows-x64-setup
SetupIconFile=..\crates\zenha-client\assets\zenha.ico
UninstallDisplayIcon={app}\{#AppExeName}
Compression=lzma
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Languages]
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autoprograms}\{#AppName} (diagnostico)"; Filename: "{app}\{#AppExeName}"; Parameters: "--diagnostico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Classes\zenha"; ValueType: string; ValueName: ""; ValueData: "URL:Zenha Protocol"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\zenha"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\zenha\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExeName},0"
Root: HKCU; Subkey: "Software\Classes\zenha\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""

[Run]
Filename: "{app}\{#AppExeName}"; Description: "Abrir o {#AppName}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#AppExeName}"; Flags: nowait skipifnotsilent
