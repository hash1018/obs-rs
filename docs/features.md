# What obs-rs does

Everything it can do, source by source. How to use it is in
[usage.md](usage.md); what it does not do yet, in [status.md](status.md).

- **Scenes and sources.** A Scene is a list of sources placed on a 1920×1080
  canvas. Move, resize, reorder, hide, lock and rename them. Duplicating a
  Scene places the same sources again rather than capturing them twice, so a
  source can appear in more than one Scene with its own position in each.
- **Display and window capture** on Windows, Linux and macOS. Frames go straight
  into GPU memory and stay there through compositing and encoding — the
  desktop never passes through system memory on its way to the recording.
  A captured window that is closed is not an error: the source waits where it
  is and picks the window up again when it comes back.
- **Video capture.** A webcam in a Scene — Media Foundation on Windows,
  Video4Linux2 on Linux, AVFoundation on macOS. The camera states which
  picture sizes and rates it offers and the Properties dock lists them, or
  leave it on Automatic and take whichever the camera puts first. A camera
  that is unplugged is not an error: the source waits and opens it again when
  it comes back, as long as it goes into the same port — the device path a
  camera is stored by contains the one it was plugged into.
- **Drawing.** A source you draw on rather than one that captures something:
  pen, highlighter and eraser, with undo and clear. It is a real layer, so
  what you draw is in the recording, not just on your screen.
- **Image.** A still picture as a source, at its own size. Decoded once
  through the same library that opens a video, so whatever FFmpeg reads is a
  source — PNG, JPEG, WebP and the rest.
- **Media file.** A video file as a source, decoded on the GPU and composited
  like anything else, with its own channel in the audio mixer. It can be set
  to start again at its end; switching that off part way through lets the
  pass it is on play out rather than stopping where it is, and the Sources
  list says when one has finished — and play starts it again. It can be
  paused and scrubbed from the Properties dock, and seeking a paused clip
  moves the picture without starting it again. It plays at 25% to 400% of
  its speed, its sound keeping its pitch, and backwards from where it is — or
  from its end — without sound, its channel dimmed until it plays forwards
  again.
- **Network stream.** An RTSP session in a Scene — an IP camera, usually —
  decoded on the GPU like a media file, with a channel in the mixer for its
  sound. A camera that stops answering is not an error: the stream is put
  back and tried again on its own, at an interval you choose, or left alone
  entirely if you turn that off. TCP or UDP, because no one transport gets
  through every network.
- **Browser.** A web page as a source: an overlay, an alert box, a chat
  window. It is rendered off-screen by Chromium; on Windows and macOS it is
  handed over as a GPU texture, so the page reaches the compositor without
  a copy through system memory, and on Linux as its pixels, which are
  copied up. Either way its transparency is real transparency, not a black
  rectangle. Address, size and frame rate are set in the Properties dock.
  Whatever the page plays gets a channel in the audio mixer, like a media
  file's sound: it is recorded, and heard as well if you switch monitoring
  on. Select the source and the Preview's toolbar grows an **Interact**
  switch: with it on, your clicks, scrolling and typing go to the page
  instead of moving the layer, which is how a page that has to be logged
  into or scrolled gets handled. Switch it off to go back to dragging the
  source around. A page nothing is showing stops drawing but stays where it
  was, so it comes back as it was; **Shut down when hidden** gives that up
  and closes the browser instead, for a heavy page in a Scene you are rarely
  on. **Refresh** loads the page again there and then, ignoring whatever was
  cached for it — what an overlay you have just edited on disk needs — and
  **Refresh when shown** does that every time the source comes back into
  view.
- **Transitions.** Switching Scenes can be nothing at all — the default, and
  what it has always been — or a fade, or a fade through black, with the
  length in milliseconds. Under the Scenes list, because it is one setting
  for the whole project. A fade brings the arriving Scene up over the one
  being left, which is exactly a dissolve wherever the arriving Scene covers
  the canvas; where it does not, fading through black is the one that is
  right. Switching again part way through ends the switch that was running
  and starts the new one.
- **A Scene inside a Scene.** Any Scene can be placed in another as a
  source: an overlay with your logo, a watermark and an alert box, kept in
  one Scene and shown in every Scene that needs it. Editing it
  once changes it everywhere. It arrives as one picture, so it is moved,
  scaled, cropped and filtered as one thing — and it is transparent where it
  draws nothing, so what is under it still shows. A Scene cannot be put
  inside itself, directly or round a loop, and deleting one that others show
  says where it is used before it goes.
- **Crop.** Alt-drag a source's handle to cut into its picture instead of
  resizing it: the dragged edge moves, the opposite one stays, and what is
  being cut off shows faintly behind while you aim. Alt+double-click puts an
  edge back, and the Properties dock takes the four numbers exactly.
- **Projector.** The Canvas on another screen, from `View → Projector`:
  a window with nothing in it but what is being composited — no selection
  outlines, no toolbar, no docks. Pick a display and it fills it, or take it
  as an ordinary window; Escape closes it, and so does the key you bound,
  which reopens it on the screen it was last on. It costs one more draw of a
  picture that exists either way, so it is a second view rather than a
  second output: nothing is encoded, and the recording does not know it is
  there.
- **Recording** to MP4, Matroska or HLS, with hardware encoding where the
  machine has it. One recording can be split into several files by elapsed
  time or by size.
- **Streaming** to any RTMP service — Twitch, YouTube, an nginx of your own.
  The server and the stream key are kept apart, because that is how services
  hand them out and because only one of the two is a secret: the key is
  masked unless you ask to see it, and never written to the log. The
  broadcast has its own encoder, bit rate, keyframe interval and height, so
  publishing at 720p while recording at 1080p is a setting rather than a
  compromise, and a broadcast that drops is opened again on its own at an
  interval you choose.
- **Filters.** On a picture: chroma key, luma key, and colour correction
  (brightness, contrast, saturation, hue, gamma and opacity). On sound:
  noise suppression, a gate, a compressor and a limiter, on a mixer channel
  or on a Source's own audio. They are added, reordered and tuned while
  everything runs — nothing is reopened, so a slider does not cost a camera
  a visible stall.
- **Replay buffer.** Keeps the last stretch of what is being composited in
  memory and writes it out when you press the key — the clip you only knew
  you wanted after it happened. It runs with or without a recording.
- **Screenshots** of the canvas, or of one Source's own picture, saved
  beside the recordings. Both are hotkeys, and unbound to begin with because
  the keys are global.
- **Audio.** Desktop and microphone channels with faders, mute, and level
  meters that read after the fader. A fader can boost past unity, and a lamp
  reports a channel that clipped. The Scene's own media files get a channel
  each — fader, mute and meter alike — so what a clip is playing can be set
  against everything else.
- **One application's sound**, on its own channel: add one with the `+` in
  the Audio Mixer and pick what is playing. The game goes to the recording
  and the chat program does not, or the other way round. It is remembered by
  the executable, so it finds the application again after a restart, and the
  channel stays where it is — with the fader and filters you set — while
  that application is closed. Note that Desktop Audio is carrying the same
  sound unless you mute it, which the picker says while you are choosing.
- **Monitoring.** Hear a media file, a stream or a microphone while you work.
  It goes to an endpoint you choose in Settings, kept separate from the one
  Desktop Audio is captured from so that what you are listening to does not
  end up in the recording twice. Switching a channel on or off does not
  interrupt what is playing. Everything monitored is still recorded — the
  switch is about your ears, not about the file. Desktop Audio has no such
  control: it is captured by listening to what your machine is already
  playing, so there is nothing there to play back.
- **Undo and redo.** `Edit → Undo` takes back the last edit, naming it —
  "Undo Move Scene 1 › Webcam" — and deleting a source comes back with its
  filters, strokes and placement. What is *operated* rather than edited is
  left alone: a fader, a mute button, a clip's play and pause, a timer.
  So is the Scene on air. An undo in another Scene does not switch to it;
  the status bar says what was taken back, and where.
- **Properties.** Selecting a source describes it in a dock of its own —
  where it sits, how large it is, and what it is actually capturing. A Color
  source's colour is edited there, and so is how see-through each placement
  is and how long it takes to fade in when shown and out when hidden — from
  the eye in the Sources dock or a key. Zero, the default, is at once.
- **A workspace that stays put.** Docks can be moved, resized and closed, and
  where they were is remembered along with the window and the Preview's zoom.
  Launching obs-rs while it is already running brings that window forward
  rather than opening a second one.
- **English and Korean**, chosen from `View → Language`.
