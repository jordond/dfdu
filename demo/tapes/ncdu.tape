# An ncdu export, browsed in dfdu.
Output out/ncdu.gif
Source tapes/_setup.tape

Hide
Type "dfdu export ~ -o /tmp/home.json && clear"
Enter
Sleep 4s
Show

Type "dfdu -f /tmp/home.json"
Enter
Sleep 1.5s
Type "5"
Sleep 2s
Screenshot out/ncdu.png
Sleep 1s
Type "q"
Sleep 500ms
