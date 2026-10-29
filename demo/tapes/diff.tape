# What grew in the last week: `dfdu diff` against a scan recorded a week earlier, then the
# browser's growth notes.
# history: home-week-ago 7d
Output out/diff.gif
Source tapes/_setup.tape

Type "dfdu diff ~ --since 7d"
Enter
Sleep 4s
Screenshot out/diff.png
Sleep 1s
Type "clear"
Enter
Type "dfdu"
Enter
Sleep 3s
Enter
Sleep 1.5s
Down
Sleep 400ms
Down
Sleep 1.5s
Type "q"
Sleep 500ms
