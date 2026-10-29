# The disk list from the command line, then the picker: choose a disk and browse it.
# mount: backup /mnt/backup
# mount: usb /media/demo/USB
Output out/disks.gif
Source tapes/_setup.tape

Type "dfdu disks"
Enter
Sleep 3s
Type "clear"
Enter
Type "dfdu --disks"
Enter
Sleep 1.5s
Down
Sleep 500ms
Down
Sleep 1s
Screenshot out/disks.png
Sleep 300ms
Enter
Sleep 3s
Down
Sleep 400ms
Enter
Sleep 1.5s
Left
Sleep 1s
Type "q"
Sleep 500ms
