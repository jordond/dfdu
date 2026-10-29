# Tree mode: directories expand in place, `*` expands the largest level by level.
Output out/tree-mode.gif
Source tapes/_setup.tape

Type "dfdu"
Enter
Sleep 3s
Type "T"
Sleep 800ms
Type "*"
Sleep 1s
Type "*"
Sleep 1s
Type "*"
Sleep 1.5s
Screenshot out/tree-mode.png
Sleep 300ms
Type "z"
Sleep 1.5s
Type "q"
Sleep 300ms
