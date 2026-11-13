# Remote control and playing together

English · [简体中文](./remote-control.zh-CN.md) · [Documentation](./README.md)

Let a friend operate your shared computer, or play a local multiplayer game
together. The Host shares an App window or screen; friends join in their browsers.

## Start remote control

1. Share through Piik App using **Apps / Windows** or **Screens**.
2. Open **Remote control** below the picture and turn it on. It starts with
   **Ask each time**.
3. Your friend selects **Request control** in their playback bar. On the Host
   page, choose **Allow** when the request arrives.

When you allow control of a shared window, Piik brings that window to the
foreground once, so the Viewer can start right away. Windows only honours this
while Piik's own page has the foreground, and input still pauses while another
window is active; switch back to the shared window to resume.

Use the App and room site from one Piik package; a page from another package is
refused rather than partly working.
Viewers need HTTPS or a localhost page for the encrypted input connection.
Ordinary LAN HTTP can play video but cannot offer remote control. Browser and
camera capture cannot receive remote input.

## Play together

1. Enable **Remote control** as above, then turn on **Allow simultaneous control**.
   This setting starts off for each source. Changing it ends current requests and
   control grants; the authorization mode and password stay in place.
2. Each friend selects **Request control**. Approve them using the same
   authorization choice as ordinary remote control.
3. In the game, assign different keyboard bindings to each player, such as
   WASD and arrow keys. The game must support sharing one keyboard.

The Host and approved friends share the computer's existing keyboard and mouse.
Each friend's held keys and mouse buttons are tracked separately, so leaving
releases only their input. Keyboard and mouse co-play needs no extra driver.
One approval gives a Viewer remote control through keyboard, mouse and an
optional supported gamepad. **Allow simultaneous control** counts approved
Viewers, not their input devices. Text insertion is available with one approved
Viewer and pauses with two or more; physical keyboard keys remain usable.
Every Viewer in the room runs the same build, so co-play is available wherever
the Host enables it.

Everyone controls the same mouse pointer; movements and clicks from approved
players apply together. Use the Host's stop action or shortcut to end control.

Approved participants have a mouse emblem on their couch avatar.
Both Host and Viewers can see who has joined; ending control removes the marker.

## Gamepads

The Host installs any required external input components from their official
source. Piik does not bundle, download or install third-party gamepad SDKs or
drivers.

Each remote player can use one or more browser-recognized controllers, each as
a separate Xbox 360 controller on a **Windows x64 Host**. Piik transports and
maps their input; the Host's optional ViGEmBus driver creates the virtual devices.

1. On the Host, install the official
   [ViGEmBus 1.22.0 release](https://github.com/nefarius/ViGEmBus/releases/tag/v1.22.0)
   and follow its restart prompt. ViGEm is end-of-life; 1.22.0 is its final
   installer and removes the old updater. If it is already installed, reuse it.
   Controllers are optional: the App offers the download in its **Remote
   control** panel when the driver is missing and says nothing further if the
   Host does not want it. Keyboard and mouse never need the driver.
2. Extract and open the updated Windows x64 Piik App package, then enable
   **Remote control**. App detects the installed driver; no DLL
   copying or SDK configuration is needed.
3. On each friend's computer, connect a standard-layout gamepad, open the
   viewing page and press a controller button so the browser recognizes it.
   Obtain remote-control approval; Piik uses every connected controller with
   the browser's standard mapping, one virtual device each. No additional
   approval is needed, and one friend may bring more than one controller.
4. Keep the viewing page focused. In the game, join as a controller player.

XInput has four controller slots in total, including controllers already
connected to the Host. Piik counts the free slots the machine actually reports,
so a slot used by a physical controller is never offered to a Viewer. When all
four are taken, the requesting Viewer is told the slots are full and can retry
once one frees; a controller never starts acting on its own. Disconnect an
unused controller or end that Viewer's remote control to free a slot. When one
Viewer brings more controllers than there are slots left, the ones already
created keep working and only the unplaced controller offers a retry. Piik does
not choose the game's player number; use the game's join and assignment controls.
Each approved Viewer uses their own virtual controller. Disconnecting the
controller removes that device while remote-control permission remains; ending remote
control releases all of that Viewer's input. Losing page or shared-window focus
returns the controller to neutral.

If the gamepad does not respond, check the Host's driver and App support, then
turn **Remote control** off and on. Keyboard/mouse co-play and ordinary remote control
remain available without the gamepad driver.

## End control

Either person can choose **End control**. The Host can stop all remote input with
**Ctrl+Alt+F10**. Change the function
key in the Host's **Remote control** panel if it conflicts with another app.
The Viewer can set a separate **Exit control shortcut** in **Remote keyboard**.
Shortcut choices are saved per site in this browser.

Changing the source or stopping sharing turns control off. After a connection
interruption, players request control again. The authorization choice is
remembered, but control never turns on automatically. A remembered **Room Viewers**
choice needs **Allow Viewers** before it applies again. Passwords are not saved.

## Mouse, touch and permissions

On a phone, touch the picture directly to click and hold to drag, without a
cursor. Two-finger tap is right-click; two-finger swipe scrolls. Pinch to zoom in
or out, then use two fingers to pan, including into the blank space around the
picture. **Playback settings → Default touch mode → Trackpad** keeps the simpler cursor
mode available.

Drag the mini mouse to reposition it; tap to expand its left and right buttons
and combined scroll wheel / middle button. The cursor stays above and to the
left of the mouse and moves with it. The picture pans at the edges to leave room
for the tool, and the cursor stops at the source picture's edges. With the mouse
expanded, use two fingers on the picture to pan or zoom while the mouse stays
in reach. Use its wheel to scroll the Host. Drag the lower shell to move; hold a
mouse button while moving to drag an item. Tap the wheel to middle-click, swipe
it to scroll, or hold then
move for a middle-button drag. The separate minimize button returns it to the mini
mouse and your selected touch mode. Leave the mini mouse at an edge to tuck it away
after the same idle delay as the player controls; tap the visible part to expand
it again. **Remote keyboard** provides keys,
shortcuts and text entry.
On a computer, **Game mouse** captures continuous mouse movement;
press **Esc** to release it.

**Room Viewers** lets authenticated room members take available control without
asking each time. **Password** requires a separate control password. Both apply
only to the current shared source. Keyboard and mouse access can operate the
computer beyond the video boundary, so authorize people you trust.

System-protected shortcuts and some games do not accept injected input.
