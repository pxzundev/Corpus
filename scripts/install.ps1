# Installs the Corpus CLI tools — corpus.exe and corpus-mcp.exe — into a folder
# you pick, then prints the mcpServers JSON for that exact path so you can paste
# it straight into a client such as pi or Claude Desktop.
$ErrorActionPreference = 'Stop'

$Default = "$env:LOCALAPPDATA\Corpus\bin"

# Locate the binaries: next to this script (a release zip), or a repo checkout.
$Candidates = @($env:CORPUS_BIN_DIR, $PSScriptRoot, "$PSScriptRoot\..\target\release", "$PWD\target\release") | Where-Object { $_ }
$BinaryDir = $null
foreach ($c in $Candidates) {
    if ((Test-Path (Join-Path $c 'corpus.exe')) -and (Test-Path (Join-Path $c 'corpus-mcp.exe'))) {
        $BinaryDir = (Resolve-Path $c).Path
        break
    }
}
if (-not $BinaryDir) {
    Write-Host "corpus.exe and corpus-mcp.exe were not found next to this script or in a nearby
target/release. Build them first (cargo build --release --workspace) or download
the CLI tools zip from the releases page and run this script from inside it."
    exit 1
}

if ($env:CORPUS_INSTALL_DIR) {
    $dest = $env:CORPUS_INSTALL_DIR
} else {
    $answer = Read-Host "Where should the Corpus CLI tools go? [$Default]"
    $dest = if ($answer) { $answer } else { $Default }
}
# Expand %VAR% references the user typed by hand.
while ($dest -match '%([^%]+)%') {
    $name = $Matches[1]
    $dest = $dest -replace ('%' + $name + '%'), ([Environment]::GetEnvironmentVariable($name))
}
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$dest = (Resolve-Path $dest).Path

try {
    Copy-Item -Force (Join-Path $BinaryDir 'corpus.exe') (Join-Path $dest 'corpus.exe')
    Copy-Item -Force (Join-Path $BinaryDir 'corpus-mcp.exe') (Join-Path $dest 'corpus-mcp.exe')
} catch {
    Write-Host "Cannot write to $dest ($($_.Exception.Message)). Re-run from an elevated
shell, or choose a folder you own, for example: `$env:CORPUS_INSTALL_DIR=`"$env:USERPROFILE\bin`" .\install.ps1"
    exit 1
}

Write-Host "Installed:"
Write-Host "  $dest\corpus.exe        index, search, list, probe"
Write-Host "  $dest\corpus-mcp.exe    stdio MCP server"

if ($env:Path -notlike "*$dest*") {
    Write-Host ""
    Write-Host "$dest is not on PATH. Add it for your user with:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$dest;`$env:Path`", 'User')"
}

Write-Host ""
Write-Host "Connect a harness or MCP client to the corpus server - add this to the"
Write-Host "client's mcpServers config (pi, Claude Desktop, and others that follow the convention):"
Write-Host ""
$mcp = @{
    mcpServers = @{
        'local-web-search' = @{ command = 'npx'; args = @('-y', '@modelcontextprotocol/server-puppeteer') }
        corpus             = @{ transport = 'stdio'; command = "$dest\corpus-mcp.exe"; args = @(); enabled = $true; timeout = 180 }
    }
}
$mcp | ConvertTo-Json -Depth 5
Write-Host ""
Write-Host "The two tools, search_docs and list_documents, run entirely against the local"
Write-Host "index - no inference engine, API key or model setup needed on this side."
Write-Host "Index documents first (corpus index <dir> or the GUI) so the tools have"
Write-Host "something to search. First run downloads the embedding models (~562 MB) once:"
Write-Host "  $dest\corpus.exe probe"
