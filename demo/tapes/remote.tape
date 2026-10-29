# Another machine's disk, scanned there by `dfdu agent` and browsed here. The agent runs in
# the same container (DFDU_REMOTE_COMMAND stands in for `ssh server dfdu agent`), against
# fixtures/server.toml at /srv.
# mount: server /srv
Output out/remote.gif
Source tapes/_setup.tape

Hide
Type 'export DFDU_REMOTE_COMMAND="dfdu agent" && clear'
Enter
Sleep 300ms
Show

Type "dfdu --remote server /srv"
Enter
Sleep 3.5s
Down
Sleep 400ms
Enter
Sleep 1s
Enter
Sleep 1.2s
Screenshot out/remote.png
Sleep 1s
Left
Sleep 600ms
Left
Sleep 600ms
Type "q"
Sleep 500ms
