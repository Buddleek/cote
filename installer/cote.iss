; Cote Windows 安装包脚本（Inno Setup 6）
; 编译：ISCC.exe installer\cote.iss
; 产物：installer\Cote-0.1.0-setup.exe

#define MyAppName "Cote"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "Cote Contributors"
#define MyAppExeName "cote.exe"

[Setup]
AppId={{8F7A2C91-5D3E-4B6A-9C48-21E0A7F5B9D2}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=.
OutputBaseFilename=Cote-{#MyAppVersion}-setup
SetupIconFile=..\assets\app.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; 普通用户可安装到自身目录（免管理员）；选择"为所有用户"时才提权
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
