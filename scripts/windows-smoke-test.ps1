# End-to-end check of the Windows installer, run by CI on a clean Windows runner (Windows
# PowerShell 5.1, for System.Speech). It installs Vaani the way a user would, downloads a
# model, dictates into Notepad by holding the hotkey (a recorded voice stands in for the
# microphone), takes screenshots, and uninstalls.
#
# usage: powershell -File scripts\windows-smoke-test.ps1 <setup.exe> <screenshot dir>
param([string]$Setup, [string]$Shots = "screenshots")
$ErrorActionPreference = "Stop"
$env:NO_COLOR = "1"
New-Item -ItemType Directory -Force $Shots | Out-Null
$Shots = (Resolve-Path $Shots).Path

Add-Type -AssemblyName System.Drawing, System.Windows.Forms, System.Speech
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Keys {
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  public static void Down(byte vk, byte scan, bool ext) { keybd_event(vk, scan, ext ? 1u : 0u, UIntPtr.Zero); }
  public static void Up(byte vk, byte scan, bool ext) { keybd_event(vk, scan, (ext ? 1u : 0u) | 2u, UIntPtr.Zero); }
}
"@

function Step($name) { Write-Host "`n=== $name" }
function Shot($name) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
  $bmp.Save("$Shots\$name.png", [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}
function Logs {
  Get-ChildItem "$env:LOCALAPPDATA\sakshamm21\vaani\data\logs" -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host "--- $($_.Name)"; Get-Content $_.FullName -Tail 80 | Write-Host }
}
function VaaniProcs { @(Get-CimInstance Win32_Process -Filter "Name = 'vaani.exe'") }
function WorkerProcs { VaaniProcs | Where-Object { $_.CommandLine -match "--speech-worker" } }
function AppProcs { VaaniProcs | Where-Object { $_.CommandLine -notmatch "--speech-worker" } }
function Fail($msg) { Shot "failure"; Logs; throw $msg }

# Bring Notepad to the front and make sure it stays there: a window that opened just before
# (the Settings window) can keep the focus, and Windows refuses SetForegroundWindow from a
# background process unless it just sent input (a Shift tap, which Notepad ignores).
function Focus($proc) {
  for ($i = 0; $i -lt 20; $i++) {
    $proc.Refresh()
    $h = $proc.MainWindowHandle
    if ($h -ne [IntPtr]::Zero) {
      [Keys]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero); [Keys]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
      [Keys]::SetForegroundWindow($h) | Out-Null
      (New-Object -ComObject WScript.Shell).AppActivate($proc.Id) | Out-Null
      Start-Sleep -Milliseconds 300
      if ([Keys]::GetForegroundWindow() -eq $h) { Start-Sleep -Milliseconds 500; return }
    }
    Start-Sleep -Milliseconds 500
  }
  Fail "could not bring Notepad to the front"
}
# Log lines on stderr are not failures: only the exit code is.
function Vaani {
  $ErrorActionPreference = "Continue"
  $out = & $script:Exe @args 2>&1 | ForEach-Object { "$_" } | Out-String
  Write-Host $out.TrimEnd()
  return $out
}

Step "Install (silent, per user)"
Start-Process $Setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
$App = "$env:LOCALAPPDATA\Programs\Vaani"
$script:Exe = "$App\bin\vaani.exe"
if (-not (Test-Path $Exe)) { Fail "vaani.exe not installed" }
if (-not (Test-Path "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Vaani.lnk")) { Fail "no Start menu shortcut" }
"{0:N0} MB installed" -f ((Get-ChildItem -Recurse $App | Measure-Object Length -Sum).Sum / 1MB)

# Only what the installer put there: no build tools or GTK on PATH.
$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot;$env:SystemRoot\System32\WindowsPowerShell\v1.0"

Step "Command line"
$v = Vaani --version
if ($v -notmatch "vaani \d") { Fail "--version printed '$v'" }
Vaani doctor | Out-Null

Step "Model download"
$m = Vaani models download moonshine-tiny-en --use
if ($LASTEXITCODE -ne 0) { Fail "model download failed" }

Step "Voice sample"
$Wav = "$env:TEMP\vaani-speech.wav"
$tts = New-Object System.Speech.Synthesis.SpeechSynthesizer
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo 16000, ([System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen), ([System.Speech.AudioFormat.AudioChannel]::Mono)
$tts.SetOutputToWaveFile($Wav, $fmt)
$tts.Speak("Hello from Vaani on Windows.")
$tts.Dispose()

Step "Start Vaani"
$env:VAANI_FAKE_MIC = $Wav
$env:VAANI_LOG = "debug"
Start-Process $Exe -ArgumentList "run"
$ready = $false
for ($i = 0; $i -lt 90; $i++) {
  Start-Sleep 1
  $s = & $Exe status 2>&1 | Out-String
  if ($s -match "ready") { $ready = $true; break }
}
Write-Host $s
if (-not $ready) { Fail "Vaani did not get ready: $s" }
if (-not (Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name Vaani -ErrorAction SilentlyContinue)) { Fail "no start-at-sign-in entry" }
# The speech worker is vaani.exe started in worker mode: no second program to allow.
if (Test-Path "$App\bin\vaani-worker.exe") { Fail "a separate vaani-worker.exe was installed" }
if (@(AppProcs).Count -ne 1 -or @(WorkerProcs).Count -ne 1) { Fail "expected one app and one worker process" }
Start-Sleep 2
Shot "1-running"

Step "Second launch opens Settings in the running app"
Start-Process $Exe -ArgumentList "settings"
Start-Sleep 4
Shot "2-settings"
if (@(AppProcs).Count -ne 1) { Fail "a second Vaani kept running" }

Step "Dictate into Notepad with the hotkey (Right Ctrl)"
$np = Start-Process notepad -PassThru
Start-Sleep 2
Focus $np
Set-Clipboard -Value "the user's clipboard"
[Keys]::Down(0xA3, 0x1D, $true)
Start-Sleep -Milliseconds 1500
Shot "3-pill-recording"
[Keys]::Up(0xA3, 0x1D, $true)
Start-Sleep -Milliseconds 300
Shot "4-pill-transcribing"
$typed = ""
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep 1
  if ((& $Exe status 2>&1 | Out-String) -match "^idle") { break }
}
Start-Sleep 2
Shot "5-notepad"
$restored = Get-Clipboard -Raw
[Keys]::Down(0x11, 0, $false); [Keys]::Down(0x41, 0, $false); [Keys]::Up(0x41, 0, $false); [Keys]::Up(0x11, 0, $false)
Start-Sleep -Milliseconds 300
Set-Clipboard -Value "(Notepad was empty)"
[Keys]::Down(0x11, 0, $false); [Keys]::Down(0x43, 0, $false); [Keys]::Up(0x43, 0, $false); [Keys]::Up(0x11, 0, $false)
Start-Sleep -Milliseconds 500
$typed = Get-Clipboard -Raw
Write-Host "Notepad has: '$typed'"
Write-Host "Clipboard after dictation: '$restored'"
Vaani history 3 | Out-Null
if ($typed -notmatch "(?i)hello.*windows") { Fail "dictated text did not reach Notepad: '$typed'" }
if ($restored -ne "the user's clipboard") { Fail "clipboard was not restored: '$restored'" }
Stop-Process $np -Force

Step "CLI toggle into Notepad"
$np = Start-Process notepad -PassThru
Start-Sleep 2
Focus $np
Vaani start | Out-Null
Start-Sleep 1
Vaani stop | Out-Null
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep 1
  if ((& $Exe status 2>&1 | Out-String) -match "^idle") { break }
}
Start-Sleep 2
[Keys]::Down(0x11, 0, $false); [Keys]::Down(0x41, 0, $false); [Keys]::Up(0x41, 0, $false); [Keys]::Up(0x11, 0, $false)
Start-Sleep -Milliseconds 300
Set-Clipboard -Value "(Notepad was empty)"
[Keys]::Down(0x11, 0, $false); [Keys]::Down(0x43, 0, $false); [Keys]::Up(0x43, 0, $false); [Keys]::Up(0x11, 0, $false)
Start-Sleep -Milliseconds 500
$typed = Get-Clipboard -Raw
Write-Host "Notepad has: '$typed'"
if ($typed -notmatch "(?i)hello.*windows") { Fail "CLI dictation did not reach Notepad: '$typed'" }
Stop-Process $np -Force

Step "Quit"
Vaani quit | Out-Null
Start-Sleep 3
if (Get-Process vaani -ErrorAction SilentlyContinue) { Fail "Vaani still running after quit" }

Step "Logs"
Get-ChildItem "$env:LOCALAPPDATA\sakshamm21\vaani\data\logs" -ErrorAction SilentlyContinue | ForEach-Object { Get-Content $_.FullName -Tail 60 }

Step "Uninstall (silent)"
Start-Process "$App\unins000.exe" -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
Start-Sleep 3
if (Test-Path $Exe) { Fail "vaani.exe still there after uninstall" }
if (Test-Path "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Vaani.lnk") { Fail "Start menu shortcut left behind" }
if (Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name Vaani -ErrorAction SilentlyContinue) { Fail "start-at-sign-in entry left behind" }
Write-Host "`nAll Windows checks passed."
