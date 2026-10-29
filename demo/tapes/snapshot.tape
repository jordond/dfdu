# A snapshot of a scan, written once and browsed later without scanning again.
Output out/snapshot.gif
Source tapes/_setup.tape

Type "dfdu export ~ --snapshot -o /tmp/home.snap"
Enter
Sleep 4s
Type "dfdu -f /tmp/home.snap"
Enter
Sleep 1.5s
Down
Sleep 400ms
Enter
Sleep 1s
Screenshot out/snapshot.png
Sleep 1s
Left
Sleep 500ms
Type "q"
Sleep 500ms
