# The README recording: browsing, the treemap and the cleanup view on the demo tree.
# Run from the repository root after `cargo build --release`:
#   scripts/vhs/demo-tree.sh /tmp/dfdu-demo && vhs scripts/vhs/browse.tape
Output assets/screenshots/browse.gif

Set Shell bash
Set FontSize 15
Set Width 1440
Set Height 900
Set Padding 16
Set TypingSpeed 60ms
Set Theme "Builtin Dark"

Hide
Type "export DFDU_STATE_DIR=$(mktemp -d) PATH=$PWD/target/release:$PATH HOME=$(cd /tmp/dfdu-demo && pwd -P) && cd ~ && clear"
Enter
Show

Type "dfdu ."
Sleep 300ms
Enter
Sleep 2s
Down
Sleep 400ms
Enter
Sleep 1s
Down
Sleep 300ms
Down
Sleep 300ms
Enter
Sleep 1.2s
Left
Sleep 500ms
Left
Sleep 800ms
Type "2"
Sleep 2.5s
Type "+"
Sleep 2s
Type "1"
Sleep 800ms
Type "3"
Sleep 2s
Enter
Sleep 1.2s
Down
Sleep 300ms
Down
Sleep 300ms
Down
Sleep 600ms
Space
Sleep 1.2s
Up
Sleep 400ms
Type "i"
Sleep 3s
Type "q"
Sleep 500ms
