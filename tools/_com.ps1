try {
  $et = New-Object -ComObject KET.Application
  'KET ok'
  $et.Quit()
} catch {
  'KET failed: ' + $_.Exception.Message
}
try {
  $wps = New-Object -ComObject KWPS.Application
  'KWPS ok'
  $wps.Quit()
} catch {
  'KWPS failed: ' + $_.Exception.Message
}
