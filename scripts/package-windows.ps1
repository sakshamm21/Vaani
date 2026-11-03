# Build the Windows downloads into dist\:
#   vaani-<version>-windows-x64-setup.exe     installer (per user, no administrator rights)
#   vaani-<version>-windows-x64-portable.zip  the same files, run bin\vaani.exe
#   SHA256SUMS-windows
#
# Needs: Rust (MSVC), the gvsbuild GTK 4 bundle in $env:GTK_DIR (default
# C:\gtk-build\gtk\x64\release), Python with pefile, and Inno Setup 6 (iscc).
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$Gtk = if ($env:GTK_DIR) { $env:GTK_DIR } else { "C:\gtk-build\gtk\x64\release" }
$Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$Dist = "dist"
$Stage = Join-Path $Dist "Vaani"

if (-not $env:SKIP_BUILD) {
  cargo build --release --locked -p vaani-app
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

if (Test-Path $Dist) { Remove-Item -Recurse -Force $Dist }
New-Item -ItemType Directory -Force "$Stage\bin" | Out-Null

# Programs and the GTK DLLs they load (found by walking their imports).
Copy-Item target\release\vaani.exe "$Stage\bin"
$Loaders = "lib\gdk-pixbuf-2.0\2.10.0"
New-Item -ItemType Directory -Force "$Stage\$Loaders\loaders" | Out-Null
Copy-Item "$Gtk\$Loaders\loaders\*.dll" "$Stage\$Loaders\loaders"
python scripts\windows-dlls.py "$Gtk\bin" "$Stage\bin" "$Stage\bin\vaani.exe" (Get-ChildItem "$Stage\$Loaders\loaders\*.dll").FullName
if ($LASTEXITCODE -ne 0) { throw "DLL copy failed" }
# The image loader list. It names the loaders under the GTK build's own prefix, which
# gdk-pixbuf maps to the install folder at run time.
& "$Gtk\bin\gdk-pixbuf-query-loaders.exe" | Set-Content -Encoding ascii "$Stage\$Loaders\loaders.cache"

# Data GTK reads at run time: settings schemas and icon themes.
New-Item -ItemType Directory -Force "$Stage\share\glib-2.0\schemas", "$Stage\share\icons" | Out-Null
Copy-Item "$Gtk\share\glib-2.0\schemas\gschemas.compiled" "$Stage\share\glib-2.0\schemas"
Copy-Item -Recurse "$Gtk\share\icons\Adwaita" "$Stage\share\icons\Adwaita"
Copy-Item -Recurse "$Gtk\share\icons\hicolor" "$Stage\share\icons\hicolor"
$Apps = "$Stage\share\icons\hicolor\scalable\apps"
New-Item -ItemType Directory -Force $Apps | Out-Null
Copy-Item assets\icons\*.svg $Apps
& "$Gtk\bin\gtk4-update-icon-cache.exe" -q -t -f "$Stage\share\icons\hicolor"
& "$Gtk\bin\gtk4-update-icon-cache.exe" -q -t -f "$Stage\share\icons\Adwaita"

# Icon, licences.
Copy-Item assets\windows\vaani.ico $Stage
New-Item -ItemType Directory -Force "$Stage\licenses" | Out-Null
Copy-Item LICENSE "$Stage\licenses\Vaani-LICENSE.txt"
Copy-Item assets\fonts\OFL-Inter.txt "$Stage\licenses\Inter-OFL.txt"
foreach ($lib in "gtk4", "glib", "pango", "cairo", "harfbuzz", "gdk-pixbuf", "librsvg", "graphene", "fribidi", "libepoxy", "pixman", "libpng", "libjpeg-turbo", "tiff", "zlib", "libffi", "pcre2", "freetype", "libxml2", "win-iconv", "gettext", "adwaita-icon-theme") {
  $d = "$Gtk\share\doc\$lib"
  if (Test-Path $d) { Copy-Item -Recurse $d "$Stage\licenses\$lib" }
}

# Portable zip.
$Zip = "$Dist\vaani-$Version-windows-x64-portable.zip"
Compress-Archive -Path $Stage -DestinationPath $Zip -CompressionLevel Optimal

# Installer.
$Iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $Iscc) { $Iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
& $Iscc /Q "/DAppVersion=$Version" packaging\windows\vaani.iss
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }

Push-Location $Dist
Get-ChildItem *.exe, *.zip | ForEach-Object {
  "{0}  {1}" -f (Get-FileHash -Algorithm SHA256 $_.Name).Hash.ToLower(), $_.Name
} | Set-Content -Encoding ascii SHA256SUMS-windows
Pop-Location
Get-ChildItem $Dist | Format-Table Name, Length
"Staged size: {0:N1} MB" -f ((Get-ChildItem -Recurse $Stage | Measure-Object Length -Sum).Sum / 1MB)
