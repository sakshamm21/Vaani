; Inno Setup script for the Windows installer. Built by scripts/package-windows.ps1, which
; stages the app in dist\Vaani first and passes the version.
;
; Per-user install into %LOCALAPPDATA%\Programs\Vaani: no administrator rights, no UAC
; prompt. Vaani itself adds the "start at sign-in" entry (Settings > General).

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif

[Setup]
AppId={{7394EA72-65E6-41C2-8DD5-89A06D35334C}
AppName=Vaani
AppVersion={#AppVersion}
AppVerName=Vaani {#AppVersion}
AppPublisher=sakshamm21
AppPublisherURL=https://github.com/sakshamm21/Vaani
AppSupportURL=https://github.com/sakshamm21/Vaani/issues
AppComments=Voice dictation: hold a key, speak, release.
DefaultDirName={localappdata}\Programs\Vaani
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\..\dist
OutputBaseFilename=vaani-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\assets\windows\vaani.ico
UninstallDisplayIcon={app}\vaani.ico
UninstallDisplayName=Vaani
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Close a running Vaani before updating its files, and start it again afterwards.
CloseApplications=force
RestartApplications=no
LicenseFile=..\..\LICENSE

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\..\dist\Vaani\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[InstallDelete]
; Files from an earlier version that this one no longer ships.
Type: filesandordirs; Name: "{app}\bin"
Type: filesandordirs; Name: "{app}\lib"
Type: filesandordirs; Name: "{app}\share"

[Icons]
Name: "{autoprograms}\Vaani"; Filename: "{app}\bin\vaani.exe"; Parameters: "settings"; IconFilename: "{app}\vaani.ico"; AppUserModelID: "io.github.sakshamm21.Vaani"; Comment: "Voice dictation"
Name: "{autodesktop}\Vaani"; Filename: "{app}\bin\vaani.exe"; Parameters: "settings"; IconFilename: "{app}\vaani.ico"; AppUserModelID: "io.github.sakshamm21.Vaani"; Tasks: desktopicon

[Registry]
; Created by Vaani at run time; removed with it.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Vaani"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\io.github.sakshamm21.Vaani"; ValueType: none; Flags: uninsdeletekey

[Run]
Filename: "{app}\bin\vaani.exe"; Parameters: "settings"; Description: "Open Vaani"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\bin\vaani.exe"; Parameters: "quit"; Flags: runhidden waituntilterminated; RunOnceId: "QuitVaani"
Filename: "{sys}\taskkill.exe"; Parameters: "/f /t /im vaani.exe"; Flags: runhidden waituntilterminated; RunOnceId: "KillVaani"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    { Settings, history and downloaded models. Kept by default (and in a silent uninstall),
      so reinstalling doesn't download the models again. }
    if SuppressibleMsgBox('Also delete your Vaani settings, history and downloaded speech models?',
         mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\sakshamm21\vaani'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\sakshamm21\vaani'), True, True, True);
    end;
  end;
end;
