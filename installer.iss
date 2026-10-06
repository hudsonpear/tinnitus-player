; Inno Setup script for Tinnitus.
; Build the binary first:  cargo build --release -p tinnitus
; Then compile:            "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer.iss
; Output lands in target\installer\.

#define AppName "Tinnitus"
#define AppVersion "1.0.0"
#define AppPublisher "Hudson Pear"
#define AppURL "https://github.com/hudsonpear/tinnitus-player"
#define AppExe "tinnitus.exe"

[Setup]
AppId={{6F3C2A1E-8B4D-4E7A-9C51-2D7E0F4A8B13}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user by default, no UAC prompt; the user can still pick all-users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=target\installer
OutputBaseFilename={#AppName}-{#AppVersion}-setup
SetupIconFile=crates\app\assets\tinnitus.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Refuse to overwrite the exe while the player is running.
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

; User library/settings in %APPDATA%\Tinnitus are left alone on uninstall on purpose.
