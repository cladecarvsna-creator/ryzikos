# Shares the Windows clipboard with RyzikOS running in QEMU.
# run-windows.bat starts this script next to QEMU; it connects to the
# serial port QEMU opens on 127.0.0.1:45577 (RyzikOS sees it as COM2).
# Text copied in Windows can be pasted in RyzikOS with Ctrl+V, and text
# copied in RyzikOS can be pasted in Windows. Only text, not pictures.
#
# Both sides send "CLIP <bytes>" on a line, then that many bytes of
# UTF-8. RyzikOS says "HELLO RYZIKOS-CLIP 1" when it starts.
# The script ends when QEMU closes.

param([int]$Port = 45577)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Get-ClipText {
    try {
        if ([System.Windows.Forms.Clipboard]::ContainsText()) {
            return [System.Windows.Forms.Clipboard]::GetText()
        }
    } catch {}
    return $null
}

function Set-ClipText([string]$text) {
    for ($i = 0; $i -lt 5; $i++) {
        try {
            if ($text.Length -eq 0) {
                [System.Windows.Forms.Clipboard]::Clear()
            } else {
                [System.Windows.Forms.Clipboard]::SetText($text)
            }
            return
        } catch {
            # another program has the clipboard open; try again shortly
            Start-Sleep -Milliseconds 100
        }
    }
}

function Send-Clip($stream, [string]$text) {
    $text = $text -replace "`r`n", "`n"
    $body = $utf8.GetBytes($text)
    $head = $utf8.GetBytes("CLIP $($body.Length)`n")
    $stream.Write($head, 0, $head.Length)
    $stream.Write($body, 0, $body.Length)
    $stream.Flush()
}

# QEMU opens the port a moment after it starts
$client = $null
for ($i = 0; $i -lt 120; $i++) {
    try {
        $client = New-Object System.Net.Sockets.TcpClient('127.0.0.1', $Port)
        break
    } catch {
        Start-Sleep -Milliseconds 500
    }
}
if ($client -eq $null) { exit 1 }
$client.NoDelay = $true
$stream = $client.GetStream()

$hello = $utf8.GetBytes("HELLO`n")
$stream.Write($hello, 0, $hello.Length)
# what both sides last agreed on, so a change isn't sent back
$last = Get-ClipText
if ($last -ne $null) { Send-Clip $stream $last }

$header = New-Object System.Collections.Generic.List[byte]
$body = $null
$left = 0
$buf = New-Object byte[] 65536

try {
    while ($client.Connected) {
        # messages from RyzikOS
        while ($stream.DataAvailable) {
            $n = $stream.Read($buf, 0, $buf.Length)
            if ($n -le 0) { throw 'closed' }
            for ($k = 0; $k -lt $n; $k++) {
                $b = $buf[$k]
                if ($left -gt 0) {
                    $body.WriteByte($b)
                    $left--
                    if ($left -eq 0) {
                        $text = $utf8.GetString($body.ToArray()) -replace "`n", "`r`n"
                        $last = $text
                        Set-ClipText $text
                    }
                } elseif ($b -eq 10) {
                    $line = $utf8.GetString($header.ToArray()).Trim()
                    $header.Clear()
                    if ($line -like '*HELLO*') {
                        # RyzikOS (re)started: give it the clipboard
                        $now = Get-ClipText
                        if ($now -ne $null) { $last = $now; Send-Clip $stream $now }
                    } elseif ($line -match 'CLIP (\d+)$') {
                        $left = [int]$Matches[1]
                        $body = New-Object System.IO.MemoryStream
                        if ($left -eq 0) { $last = ''; Set-ClipText '' }
                    }
                } elseif ($header.Count -lt 256) {
                    $header.Add($b)
                }
            }
        }
        # new text copied in Windows
        $now = Get-ClipText
        if ($now -ne $null -and $now -ne $last) {
            $last = $now
            Send-Clip $stream $now
        }
        # a closed connection shows up as readable with nothing to read
        if ($client.Client.Poll(0, [System.Net.Sockets.SelectMode]::SelectRead) -and $client.Available -eq 0) {
            break
        }
        Start-Sleep -Milliseconds 250
    }
} catch {
} finally {
    $client.Close()
}
