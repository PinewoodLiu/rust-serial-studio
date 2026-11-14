param([string]$OutputName = '串口助手.exe')
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
$taskCargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($taskCargo) { $taskCargoPath = $taskCargo.Source }
else { $taskCargoPath = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path -LiteralPath $taskCargoPath)) { throw '未找到 Rust，请先安装 Rust stable 和 Visual Studio C++ 生成工具。' }
& $taskCargoPath build --release --locked
if ($LASTEXITCODE -ne 0) { throw '构建失败。' }
New-Item -ItemType Directory -Path (Join-Path $PSScriptRoot 'dist') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\serial-assistant.exe') -Destination (Join-Path (Join-Path $PSScriptRoot 'dist') $OutputName) -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.md') -Destination (Join-Path $PSScriptRoot 'dist\使用说明.md') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'LICENSE') -Destination (Join-Path $PSScriptRoot 'dist\LICENSE') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'THIRD_PARTY_NOTICES.md') -Destination (Join-Path $PSScriptRoot 'dist\THIRD_PARTY_NOTICES.md') -Force
Write-Host "已生成 dist\$OutputName"

