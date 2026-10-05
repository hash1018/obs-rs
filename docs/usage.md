# Using obs-rs

Adding and arranging sources, the docks, recording and streaming, and the
keys. What each kind of source can do is in [features.md](features.md).

Add a source from the **+** under the Sources dock. A Display Capture asks
which monitor and a Window Capture which window; on Wayland the system's own
picker opens instead, and the choice is remembered so later runs do not ask
again.

A Window Capture is stored as the window's program and title rather than as a
handle, which only means something while that window is open. Closing the
window empties the layer and reopening it fills it again — a browser you quit
for the day is still in the Scene tomorrow.

Drag a source in the Preview to move it and its handles to resize it. Sources
higher in the list are drawn in front of lower ones.

The Preview marks what it is showing you. The selected source is outlined in
red, with the distance from each of its four edges to the edge of the canvas
written alongside — the numbers for placing something exactly, centred or
flush, instead of doing that arithmetic by eye. An edge that a crop cut is
drawn in a green dashed line instead, so a cropped source does not read as
merely a smaller one. Whatever hangs outside the canvas is hatched, being in
the Scene but not in the recording. And the source under the pointer is
outlined thinly in blue, which is the only way to tell what a click would take
where one source covers another.

Double-click a source's name in the dock to change it, the same way a Scene's
name is changed. The name belongs to the source rather than to the Scene, so
one placed in two Scenes is renamed in both at once; a name already taken is
refused where you typed it, and Escape leaves the old one alone.

Select a **Drawing** and the Preview's toolbar grows a pen. It stays in Select
until you pick one, so a stray click cannot leave a mark. The eraser takes
whole strokes rather than rubbing at pixels, and undo is the same thing aimed
at the last one.

The **Properties** dock says what the selected source is: its name and kind,
where it sits on the canvas and how large, and what it captures — the monitor
and its place in the desktop, or the program and title of a window. What a
source can be told is there too: a Color's colour, a camera's picture size
and rate, a page's address, a clip's playback and scrub bar, a stream's
transport and reconnect interval, and the four crop numbers exactly.

The **Filters** dock is the selected source's own chain, under a **Picture**
tab and a **Sound** one: a chroma key, a luma key or colour correction on
what it shows, and noise suppression, a gate, a compressor or a limiter on
what it plays. Buttons move a filter earlier or later in the chain, and
switching one off does not cost it what it was tuned to. They belong to the
source rather than to the Scene, so one placed in two Scenes is filtered in
both. A mixer channel has a chain of its own, reached from the channel.
Everything is applied while it runs: no slider reopens a camera.

**Start Recording** writes to your Videos folder unless Settings says
otherwise. What it records is the canvas, not the window — the selection
outlines and the pen toolbar are editor-only and never appear in the file.

**Start Streaming** needs a server and a stream key in
**Settings → Streaming**. The Controls dock says when a broadcast is live and
the status bar counts how long it has been; a broadcast that drops says so
and comes back on its own unless you turned that off. Recording and streaming
are independent — either, both, or one started part way through the other.

The **replay buffer** keeps the last stretch of the canvas in memory rather
than writing it: start it from the Controls dock, and the key you bound
writes what it holds to a file beside the recordings. It says how many
seconds it is holding, and that it cannot save anything in its first second.

**Start Virtual Camera** (Windows 11) shows the canvas as a camera other
applications can open, until you press it again or close obs-rs. Pick
"obs-rs" in the other application's camera list.

Keys: `Ctrl+R` starts and stops recording, `Ctrl+P` pauses and resumes one,
`Ctrl+1` … `Ctrl+9` switch to that Scene, `F11` goes fullscreen, `Ctrl+,`
opens Settings, and `Ctrl+Z` / `Ctrl+Y` undo and redo — on a Mac, `⌘` in
place of `Ctrl`, and the menus in the menu bar at the top of the screen.
Starting and stopping a broadcast, running the replay buffer and saving
from it, the two screenshots, and opening and closing the projector have
keys of their own, unbound to begin with. None of them fire while you are
typing a name.

All but the Scene keys can be changed in **Settings → Hotkeys**: click a
binding, press the key you want, or Backspace to clear it. A key you choose
works while another application has focus, which is the point of it — a
recorder has to be reachable from inside the game it is recording. On
Windows that is a thread asking the system which bound keys are down, and
on macOS the same once obs-rs is allowed Input Monitoring in System
Settings, which it asks for when you first bind one; on Linux it is the
desktop's own global-shortcuts portal, which asks you once whether to allow
them. The keys obs-rs comes with work only in its own window: `Ctrl+R` and
`Ctrl+P` are every browser's and editor's own, and heard everywhere they
would start or pause a recording from inside something else. Each mixer
channel gets a push-to-talk and a push-to-mute binding on that page too, which is why a key has to report being let go as well as
pressed. Every Scene has a key of its own there, and so does every source
in one: one key shows or hides that placement, so the same source in two
Scenes is two keys rather than one that takes both.

**File → Show Recordings** opens that folder in your file manager, and
**File → Settings** is the same dialog the Controls dock's button opens —
there because that dock can be closed.

Settings has six pages: General, Video (output resolution and frame rate),
Audio (sample rate, channels, and where monitoring is played), Recording
(where files go, format, encoder, bit rates, splitting, the replay buffer),
Streaming (server, key, its own encoder and bit rates, reconnecting), and
Hotkeys.
