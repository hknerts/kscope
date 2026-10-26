# Records assets/demo.gif with https://github.com/charmbracelet/vhs.
#
#   make dev-up     # the four kind clusters this demo browses
#   make demo       # cargo build --release, then: vhs hack/demo.tape
#
# The tape drives the release binary from target/release, so what is recorded
# is what is in the working tree. Timings assume the dev clusters are warm
# (run kscope against them once first); stretch the Sleeps on a slow machine.
# If a key binding changes, update the step here too: the tape is the script.

Output assets/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1400
Set Height 800
Set Padding 16
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms
Set Framerate 24

Hide
Type "export PATH=$PWD/target/release:$PATH && clear"
Enter
Show

# A fleet: both staging clusters listed as one, with a context column.
Type "kscope -u stg -n shop"
Sleep 400ms
Enter
Sleep 3s

# Only the objects in trouble: the crash-looping pod on each cluster.
Type "!"
Sleep 2.5s
Type "!"
Sleep 1s

# Switch to deployments with the k9s-style palette.
Type ":deploy"
Sleep 600ms
Enter
Sleep 2s

# Open checkout: its replicas' logs from every cluster, merged.
Type "/checkout"
Sleep 500ms
Enter
Sleep 800ms
Enter
Sleep 4s

# Search the merged stream for errors.
Type "/ERROR"
Sleep 400ms
Enter
Sleep 2.5s

# Metrics, then events, of the same deployment.
Type "m"
Sleep 3s
Type "e"
Sleep 2.5s

# Back to the list, and the deployment compared across the fleet.
Escape
Sleep 1s
Type "a"
Sleep 4s
Escape
Sleep 1s

Type "q"
Sleep 500ms
