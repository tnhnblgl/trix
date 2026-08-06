Trix 0.4.0
==========

A clip recorder for Windows, for people whose PC cannot spare the frames.
Trix keeps the last few seconds of your screen in memory and writes them to
a file when you press a key. Capture and encoding run on the GPU, so the
cost to your game is close to nothing, and the app itself stays out of the
way while you play.


NEW IN 0.4.0 -- PLEASE READ
---------------------------

Trix now records your MICROPHONE, and it is ON by default at full level.

If you upgraded from 0.3.0, the next clip you save will contain your voice.
Nothing warns you first, and Windows will show its microphone indicator
whenever Trix is recording.

To turn it off:  Settings -> Audio -> Microphone level -> drag to 0.

Zero is a real off switch, not a mute. At 0 Trix never opens the microphone
at all, so the Windows recording indicator stays dark and no other app sees
Trix holding your input device.

Also new:

  * Settings -> Audio has two sliders, one for PC sound and one for the
    microphone. 100 is full volume; Trix never amplifies past the original.
  * Changing a level takes effect on the next clip. Turning a source from
    off to on, or on to off, needs a re-arm -- the app tells you when.


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
  Hotkey            Alt+F10
  Clips folder      %USERPROFILE%\Videos\Trix
  Library cap       20 GB, oldest clips deleted first
  GPU priority      low -- your game gets the GPU first
  PC sound          100
  Microphone        100
  Start with Windows  off

All of these are in Settings.


WHERE THINGS LIVE
-----------------

  Clips     %USERPROFILE%\Videos\Trix   (changeable in Settings)
  Settings  %APPDATA%\trix\config.toml


KNOWN LIMITS IN THIS RELEASE
----------------------------

  * No installer yet. Unzip it where you want it; an MSI is planned.

  * If Alt+F10 does nothing, something else already owns it -- the NVIDIA
    overlay claims it on many machines. Settings -> Hotkey lets you pick
    another combination, and the Test button there tells you whether Trix
    actually received your key.

  * No trimming or exporting yet. Clips are saved whole, at the length you
    set. That is the next thing being built.

  * Trix can only clip while the tray recorder is running, and opening the
    app is what starts it. If you would rather have it ready at login
    without opening the app, turn on "Start with Windows" in Settings --
    that starts the tray recorder, not the window.


LICENSE
-------

MIT. See the LICENSE file.

Source, issues and newer releases:  https://github.com/tnhnblgl/trix
