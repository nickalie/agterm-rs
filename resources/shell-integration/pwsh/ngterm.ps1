# ngterm's shell integration for PowerShell 7 (pwsh) and Windows PowerShell 5.1, which Ghostty does not integrate:
# what Ghostty's own integrations (src/shell-integration at LIBGHOSTTY_VT_REV, build/pins.env) do for zsh, bash and
# fish. scripts/stage.sh stages it beside them, as share/ghostty/shell-integration/pwsh/ngterm.ps1.
#
# A live pane's PowerShell loads it from `-NoExit -Command`, after its profiles, as a script block
# (crates/agterm-core/src/live/shell_integration.rs), so it wraps the profile's prompt instead of replacing it:
#
# - OSC 133 marks: A where the prompt starts, B where its input starts, C when a command line is accepted (through
#   PSReadLine, when it is loaded), D with the last command's status before the next prompt;
# - OSC 7 with the working directory, in Ghostty's kitty-shell-cwd form; a Windows path is written with `/` after a
#   leading one (`/C:/Users/me`);
# - with `title` in GHOSTTY_SHELL_FEATURES: the directory as the title at the prompt, the command line while it runs;
# - with `cursor` there: a blinking bar cursor at the prompt (a steady one with `cursor:steady`), the default shape
#   back while a command runs.
#
# Loading it twice changes nothing. It uses only language syntax, .NET calls and module-qualified cmdlets, so it runs
# under Windows PowerShell 5.1 as well and no alias or function of the user's takes it over.

if ($global:__NgtermIntegration) { return }

# built in a scope of its own, so no variable but this one is left behind
$global:__NgtermIntegration = & {
    $features = @("$env:GHOSTTY_SHELL_FEATURES".Split(','))
    $cursor = $null
    if ($features -like 'cursor*') {
        $cursor = if ($features -contains 'cursor:steady') { 6 } else { 5 }
    }
    @{
        # the prompt and the line reader this script wraps
        Prompt   = $function:prompt
        ReadLine = $function:PSConsoleHostReadLine
        # whether a C mark waits for its D
        Running  = $false
        Title    = $features -contains 'title'
        Cursor   = $cursor
        # the host name the terminal compares with its own: Windows' computer name, gethostname elsewhere
        HostName = if ($env:OS -eq 'Windows_NT') { $env:COMPUTERNAME } else { [System.Net.Dns]::GetHostName() }
    }
}

function global:prompt {
    # first, before any statement replaces them
    $succeeded = $?
    $exitCode = $global:LASTEXITCODE
    $state = $global:__NgtermIntegration
    $esc = [char]27
    $bel = [char]7
    $marks = ''
    if ($state.Running) {
        $status = if ($succeeded) { 0 } elseif ($exitCode -is [int] -and $exitCode -ne 0) { $exitCode } else { 1 }
        $marks += "$esc]133;D;$status$bel"
        $state.Running = $false
    }
    $location = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem') {
        $path = $location.ProviderPath.Replace('\', '/')
        if (-not $path.StartsWith('/')) { $path = "/$path" }
        $marks += "$esc]7;kitty-shell-cwd://$($state.HostName)$path$bel"
    }
    if ($state.Title) {
        $title = $location.Path
        if ($HOME -and $title.StartsWith($HOME, [System.StringComparison]::OrdinalIgnoreCase) -and
            ($title.Length -eq $HOME.Length -or '\/'.Contains($title[$HOME.Length]))) {
            $title = '~' + $title.Substring($HOME.Length)
        }
        $marks += "$esc]2;$($title -replace '[\x00-\x1f\x7f]', '')$bel"
    }
    if ($state.Cursor) { $marks += "$esc[$($state.Cursor) q" }
    # the wrapped prompt sees the command's status: its exit code, and $? false through an ignored error
    $global:LASTEXITCODE = $exitCode
    if (-not $succeeded) { Microsoft.PowerShell.Utility\Write-Error -Message 'failed' -ErrorAction Ignore }
    $text = if ($state.Prompt) {
        -join @(& $state.Prompt)
    } else {
        "PS $location$('>' * ($NestedPromptLevel + 1)) "
    }
    $global:LASTEXITCODE = $exitCode
    "$marks$esc]133;A$bel$text$esc]133;B$bel"
}

if ($global:__NgtermIntegration.ReadLine) {
    function global:PSConsoleHostReadLine {
        $state = $global:__NgtermIntegration
        $line = -join @(& $state.ReadLine)
        $esc = [char]27
        $bel = [char]7
        $marks = ''
        if ($state.Cursor) { $marks += "$esc[0 q" }
        if ($line.Trim()) {
            if ($state.Title) { $marks += "$esc]2;$($line -replace '[\x00-\x1f\x7f]', '')$bel" }
            $marks += "$esc]133;C$bel"
            $state.Running = $true
        }
        [System.Console]::Write($marks)
        $line
    }
}
