$root = "E:\workspace\rust\poemreader"
try {
  $et = New-Object -ComObject KET.Application
  $et.Visible = $false
  $et.DisplayAlerts = $false
  $wb = $et.Workbooks.Open("$root\samples\sample.xlsx")
  $wb.SaveAs("$root\samples\_legacy.xls", 56)
  $wb.Close($false)
  $et.Quit()
  'xls written'
} catch {
  'xls failed: ' + $_.Exception.Message
}
try {
  $wps = New-Object -ComObject KWPS.Application
  $wps.Visible = $false
  $wps.DisplayAlerts = $false
  $doc = $wps.Documents.Open("$root\samples\sample.docx")
  $doc.SaveAs2("$root\samples\_legacy-sample.doc", 0)
  $doc.Close(0)
  $wps.Quit()
  'doc written'
} catch {
  'doc failed: ' + $_.Exception.Message
}
Get-ChildItem "$root\samples\_legacy.xls","$root\samples\_legacy-sample.doc" -ErrorAction SilentlyContinue |
  Select-Object Name, Length
