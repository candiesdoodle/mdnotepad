; MdNotePad installer: per user (no admin rights), built by package.ps1
; with Inno Setup 6.  The tasks register the file types and the preview
; pane's handler through MdNotePad.exe's own command-line options, and the
; uninstaller removes both.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif

[Setup]
AppId={{B9E5A4C2-6F1D-4E8A-9C3B-2D7F1E0A5B64}
AppName=MdNotePad
AppVersion={#AppVersion}
AppVerName=MdNotePad {#AppVersion}
AppPublisher=candiesdoodle
AppPublisherURL=https://github.com/candiesdoodle/mdnotepad
AppSupportURL=https://github.com/candiesdoodle/mdnotepad/issues
DefaultDirName={autopf}\MdNotePad
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=MdNotePad-{#AppVersion}-setup
SetupIconFile=..\crates\rustdown-gui\windows\mdnotepad.ico
UninstallDisplayIcon={app}\MdNotePad.exe
UninstallDisplayName=MdNotePad
LicenseFile=..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Explorer's preview host keeps the preview DLL open: close it (or replace
; the DLL on the next restart) when updating.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "fileassoc"; Description: "Open .md, .markdown and .txt files with MdNotePad"; GroupDescription: "Windows integration:"
Name: "preview"; Description: "Show .md files formatted in File Explorer's preview pane"; GroupDescription: "Windows integration:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\MdNotePad.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\mdnotepad_preview.dll"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"

[Icons]
Name: "{autoprograms}\MdNotePad"; Filename: "{app}\MdNotePad.exe"
Name: "{autodesktop}\MdNotePad"; Filename: "{app}\MdNotePad.exe"; Tasks: desktopicon

[Run]
; In this order: unregistering the file types also removes the preview handler.
Filename: "{app}\MdNotePad.exe"; Parameters: "--unregister-file-types"; Tasks: not fileassoc; Flags: runhidden waituntilterminated
Filename: "{app}\MdNotePad.exe"; Parameters: "--register-file-types"; Tasks: fileassoc; Flags: runhidden waituntilterminated
Filename: "{app}\MdNotePad.exe"; Parameters: "--unregister-preview"; Tasks: not preview; Flags: runhidden waituntilterminated
Filename: "{app}\MdNotePad.exe"; Parameters: "--register-preview"; Tasks: preview; Flags: runhidden waituntilterminated
Filename: "{app}\MdNotePad.exe"; Description: "{cm:LaunchProgram,MdNotePad}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\MdNotePad.exe"; Parameters: "--unregister-file-types"; Flags: runhidden waituntilterminated; RunOnceId: "Unregister"
