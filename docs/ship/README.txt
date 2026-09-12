Trix 1.3.0
==========

A clip recorder for Windows, for people whose PC cannot spare the frames.
Trix keeps the last few seconds of your screen in memory and writes them to
a file when you press a key. Capture and encoding run on the GPU, so the
cost to your game is close to nothing, and the app itself stays out of the
way while you play.


NEW IN 1.3.0
------------

The two volume sliders now go up to 200. They still start at 100, which
is the sound exactly as your PC mixed it -- nothing changes unless you
move them.

Anything above 100 makes Trix amplify. This is for the case the old limit
could not help with: a microphone that comes out too quiet in the clip
even with Windows already at full input level. Turning Trix up is the
only thing left that can fix that. Pushed far enough it will distort, the
same way any recording does, so raise it until your voice sits right and
no further.

There is also a new setting: "Arm when Trix starts". With it on, Trix
begins filling the replay buffer the moment it opens, so there is nothing
to remember to switch on. It is worth pairing with "Start with Windows" --
together they mean Trix is recording from the time you log in.

It is off by default, because arming holds your GPU encoder and the
buffer's memory for as long as it lasts, and that is not something to
switch on for you. It takes effect the next time Trix starts, not the
moment you tick it.

Both settings are in Settings -> Audio and Settings -> Trix.


GETTING STARTED
---------------

  1. Unzip everything into one folder and keep the files together.
     The app looks for the recorder right beside itself.

  2. Run  trix-ui.exe.  It starts the background recorder for you.

  3. Press Start in the app. The tray icon shows when Trix is recording.

  4. Press Alt+F10 during your game to save the last 15 seconds.

Windows SmartScreen will probably warn you the first time, because these
binaries are not code-signed. "More info" -> "Run anyway".


WHAT IS IN THIS FOLDER
----------------------

  trix-ui.exe      The app: your clips, playback and settings.
  trix-daemon.exe  The background recorder that lives in the tray. The app
                   starts it, so you do not normally run this yourself.
  trix.exe         A command-line interface, for scripting and diagnostics.
  LICENSE          MIT.
  THIRD-PARTY-NOTICES.txt
                   Licences for material Trix includes (its icons).


REQUIREMENTS
------------

  * Windows 10 or 11, 64-bit.
  * A GPU with a hardware H.264 encoder -- any Intel, AMD or NVIDIA graphics
    from roughly the last decade qualifies.

No runtime to install. The Visual C++ libraries are linked in.


DEFAULTS
--------

  Replay length     15 seconds
  Frame rate        60 fps
  Bitrate           8000 kbps, variable
  Clip hotkey       Alt+F10
  Screenshot hotkey Alt+F8
  Clips folder      %USERPROFILE%\Videos\Trix
  Library cap       off -- Trix never deletes a clip to save space
  GPU priority      low -- your game gets the GPU first
  PC sound          100  (0-200 -- above 100 amplifies)
  Microphone        100  (0-200 -- above 100 amplifies)
  Start with Windows  off
  Arm when Trix starts  off
  Discord presence  on

All of these are in Settings.


WHERE THINGS LIVE
-----------------

  Clips     %USERPROFILE%\Videos\Trix   (changeable in Settings)
  Settings  %APPDATA%\trix\config.toml


UPDATES
-------

Trix asks github.com once, when you open the app, whether a newer version
exists. It sends nothing about you -- no account, no identifier, no
information about your clips or your PC.

When there is one, a bar appears at the top of the window. Nothing is
downloaded or installed until you click Update. Trix then replaces itself
and restarts, keeping your settings and your clips.

To turn the check off:  Settings -> Updates -> Check for updates.

Trix cannot update itself if you put it somewhere Windows protects, such as
Program Files. It will say so and point you at the download page.

If you ever update by unzipping into a *new* folder rather than letting Trix
update itself, check "Start with Windows" afterwards. Windows remembers the
old folder, so it would go on starting the copy you replaced. Trix notices
this and shows the setting as off; switching it back on points Windows here.


SCREENSHOTS
-----------

Alt+F8 saves a picture of the screen, the way Alt+F10 saves a clip. It goes
to a Screenshots folder inside your clips folder, and onto your clipboard at
the same time, so you can paste it straight into Discord or a chat window
without going to find the file first.

Trix has to be recording. The picture comes off the same live capture the
clips come off, so with Trix stopped there is nothing to take a picture of,
and the key says so rather than doing nothing. Press Start first.

The Screenshots tab shows everything you have taken. Click one to see it
full size; copy, show in folder and delete are on the card.

A screenshot plays a different sound from a clip, so you can tell which key
you hit without looking. Settings -> Trix has a switch for it, and the
Screenshot hotkey is there too if something else on your PC already owns
Alt+F8.


CLIP SOUND
----------

Trix plays a short sound when it saves a clip, so you know the hotkey worked
without leaving your game. It plays whether or not the Trix window is open.

To use your own:  Settings -> Trix -> Sound file -> Choose...

mp3, wav, m4a and anything else Windows can play will work. Only the first
10 seconds are used. Trix converts your file once and keeps its own copy, so
moving or deleting the original later will not stop the sound.

Reset puts Trix's own sound back. To turn the sound off entirely, use the
Clip sound switch just above it.

There is no volume slider. Use Windows' volume mixer to set how loud
trix-daemon.exe is.


KNOWN LIMITS IN THIS RELEASE
----------------------------

  * No installer yet. Unzip it where you want it; an MSI is planned. Trix
    still updates itself in place, so you only download it by hand once.

  * If Alt+F10 does nothing, something else already owns it -- the NVIDIA
    overlay claims it on many machines. Settings -> Hotkey lets you pick
    another combination, and the Test button there tells you whether Trix
    actually received your key.

  * Trimming is fast mode only. The in point lands on the last keyframe at
    or before where you put it -- about a second's grain in clips Trix
    recorded, and possibly further back in an .mp4 from somewhere else.
    Frame-accurate trimming, for starting on an exact frame, is not in yet.

  * Trix can only clip while the tray recorder is running, and opening the
    app is what starts it. If you would rather have it ready at login
    without opening the app, turn on "Start with Windows" in Settings --
    that starts the tray recorder, not the window.

  * Messages from Trix -- a clip saved, an export finished, something
    that went wrong -- are not drawn over a video playing fullscreen.
    They clear themselves after a few seconds, so one raised while you
    are fullscreen is missed rather than delayed.


LICENSE
-------

MIT. See the LICENSE file.

Source, issues and newer releases:  https://github.com/tnhnblgl/trix
