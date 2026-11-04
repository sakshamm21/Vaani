# End-to-end check of the Microsoft Store package on a clean Windows runner (Windows
# PowerShell 5.1). The Store signs the real package; here it is signed with a throwaway test
# certificate so Windows will install it. Installs it, starts Vaani from the Start menu entry,
# dictates into Notepad through the `vaani` command alias, and removes the package.
#
# usage: powershell -File scripts\windows-msix-test.ps1 <package.msix>
param([string]$Msix)
$ErrorActionPreference = "Stop"
$env:NO_COLOR = "1"
Add-Type -AssemblyName System.Speech
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Keys {
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public static void Chord(byte mod, byte vk) { keybd_event(mod, 0, 0, UIntPtr.Zero); keybd_event(vk, 0, 0, UIntPtr.Zero); keybd_event(vk, 0, 2, UIntPtr.Zero); keybd_event(mod, 0, 2, UIntPtr.Zero); }
}
"@
function Step($name) { Write-Host "`n=== $name" }
function Logs {
  Get-ChildItem "$env:LOCALAPPDATA\Packages\*\LocalCache\Local\sakshamm21\vaani\data\logs", "$env:LOCALAPPDATA\sakshamm21\vaani\data\logs" -ErrorAction SilentlyContinue |
    Get-ChildItem | ForEach-Object { Write-Host "--- $($_.FullName)"; Get-Content $_.FullName -Tail 60 | Write-Host }
}
function Fail($msg) { Logs; throw $msg }
function Vaani {
  $ErrorActionPreference = "Continue"
  $out = & $script:Alias @args 2>&1 | ForEach-Object { "$_" } | Out-String
  Write-Host $out.TrimEnd()
  return $out
}

Step "Sign with a test certificate and install"
$Msix = (Resolve-Path $Msix).Path
# Sign with the publisher the package names (a Store identity works the same).
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($Msix)
$reader = New-Object IO.StreamReader ($zip.GetEntry("AppxManifest.xml").Open())
[xml]$manifest = $reader.ReadToEnd()
$reader.Close(); $zip.Dispose()
$Name = $manifest.Package.Identity.Name
$Publisher = $manifest.Package.Identity.Publisher
Write-Host "package $Name, publisher $Publisher"
$cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher -KeyUsage DigitalSignature `
  -CertStoreLocation Cert:\CurrentUser\My -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
$pfx = "$env:TEMP\vaani-test.pfx"
$pw = ConvertTo-SecureString -String "test" -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $pw | Out-Null
Import-PfxCertificate -FilePath $pfx -CertStoreLocation Cert:\LocalMachine\TrustedPeople -Password $pw | Out-Null
$SignTool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object FullName -Descending | Select-Object -First 1
& $SignTool.FullName sign /fd SHA256 /f $pfx /p test $Msix | Out-Null
if ($LASTEXITCODE -ne 0) { throw "signtool failed" }
Add-AppxPackage -Path $Msix
$pkg = Get-AppxPackage -Name $Name
if (-not $pkg) { Fail "package not installed" }
Write-Host "$($pkg.PackageFullName) in $($pkg.InstallLocation)"
$script:Alias = "$env:LOCALAPPDATA\Microsoft\WindowsApps\vaani.exe"
if (-not (Test-Path $Alias)) { Fail "no vaani command alias" }

Step "Command line through the alias"
$v = Vaani --version
if ($v -notmatch "vaani \d") { Fail "--version printed '$v'" }
Vaani doctor | Out-Null
$m = Vaani models download moonshine-tiny-en --use
if ($LASTEXITCODE -ne 0) { Fail "model download failed" }

Step "Start from the Start menu entry (opens Settings)"
$Wav = "$env:TEMP\vaani-msix-speech.wav"
$tts = New-Object System.Speech.Synthesis.SpeechSynthesizer
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo 16000, ([System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen), ([System.Speech.AudioFormat.AudioChannel]::Mono)
$tts.SetOutputToWaveFile($Wav, $fmt)
$tts.Speak("Hello from Vaani on Windows.")
$tts.Dispose()
function WaitReady {
  for ($i = 0; $i -lt 90; $i++) {
    Start-Sleep 1
    $s = & $Alias status 2>&1 | Out-String
    if ($s -match "ready") { Write-Host $s; return }
  }
  Fail "Vaani did not get ready: $s"
}
Start-Process explorer.exe "shell:AppsFolder\$($pkg.PackageFamilyName)!Vaani"
WaitReady
$procs = @(Get-CimInstance Win32_Process -Filter "Name = 'vaani.exe'")
$procs | ForEach-Object { Write-Host "$($_.ProcessId) $($_.CommandLine)" }
if (-not ($procs | Where-Object { $_.ExecutablePath -match "WindowsApps" })) { Fail "Vaani is not running from the package" }
Add-Type -AssemblyName UIAutomationClient
$win = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
  [System.Windows.Automation.TreeScope]::Children,
  (New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::NameProperty), "Vaani"))
if (-not $win) { Fail "the Start menu entry did not open the Settings window" }
Vaani quit | Out-Null
Start-Sleep 3

Step "Start through the vaani command, with a recorded voice for the microphone"
# The Start menu launch doesn't inherit this shell's environment; the command alias does.
$env:VAANI_FAKE_MIC = $Wav
Start-Process $Alias -ArgumentList "run"
WaitReady

Step "Dictate into Notepad"
$np = Start-Process notepad -PassThru
Start-Sleep 3
(New-Object -ComObject WScript.Shell).AppActivate($np.Id) | Out-Null
Start-Sleep 1
Vaani start | Out-Null
Start-Sleep 1
Vaani stop | Out-Null
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep 1
  if ((& $Alias status 2>&1 | Out-String) -match "^idle") { break }
}
Start-Sleep 2
[Keys]::Chord(0x11, 0x41)
Start-Sleep -Milliseconds 300
Set-Clipboard -Value "(Notepad was empty)"
[Keys]::Chord(0x11, 0x43)
Start-Sleep -Milliseconds 500
$typed = Get-Clipboard -Raw
Write-Host "Notepad has: '$typed'"
if ($typed -notmatch "(?i)hello.*windows") { Fail "dictation did not reach Notepad: '$typed'" }
Stop-Process $np -Force

Step "Quit and remove the package"
Vaani quit | Out-Null
Start-Sleep 3
if (Get-Process vaani -ErrorAction SilentlyContinue) { Fail "Vaani still running after quit" }
Logs
Remove-AppxPackage -Package $pkg.PackageFullName
if (Get-AppxPackage -Name $Name) { Fail "package still installed" }
Write-Host "`nStore package checks passed."
