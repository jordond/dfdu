# The headless summary, and disk usage by file type.
Output out/scan-cli.gif
Source tapes/_setup.tape

Type "dfdu scan ~ --depth 1"
Enter
Sleep 4s
Type "clear"
Enter
Type "dfdu scan ~ --by-type"
Enter
Sleep 4s
Screenshot out/scan-cli.png
Sleep 300ms
