Trix 0.8.0
==========

A clip recorder for Windows, for people whose PC cannot spare the frames.
Trix keeps the last few seconds of your screen in memory and writes them to
a file when you press a key. Capture and encoding run on the GPU, so the
cost to your game is close to nothing, and the app itself stays out of the
way while you play.


NEW IN 0.8.0
------------

Trix has been redrawn. Every control in the app -- the switches, the
sliders, the dropdowns, the boxes you type numbers into -- used to be
whatever the browser engine inside Trix decided to draw, and none of
them answered the pointer. They are Trix's own now: they light up as
the pointer crosses them, press down when you click, and show a ring
when you reach them from the keyboard.

The grey Windows title bar is gone. Trix draws its own, and the Start
control sits in it, so whether Trix is recording is on screen no
matter which page you are looking at.

Settings is grouped into panels instead of sixteen rows in one long
scroll, and the unit of a setting -- seconds, fps, kbps -- now sits on
the box you type into rather than in a line of help text underneath.

Clips look like clips. A card shows the picture, how long it runs,
what it is called, and its size, resolution and frame rate; a star
marks the ones you keep. Favourite, Rename and Delete have moved into
a small menu on the card, so renaming a clip no longer means opening
it first.

The clip page has one timeline instead of two. The scrubber you drag
to watch and the In and Out handles you drag to trim are now the same
track, so the point you are looking at and the point you are cutting
at are the same point. I and O still set the two trim points, and the
arrow keys still move the playhead.

Nothing about recording changed, and nothing about your clips or your
settings did. Same hotkey, same files, same folders -- this release
changes what Trix looks like, not what it does.


NEW IN 0.7.0
------------

You can trim a clip. Open one, drag the In and Out handles on the bar under
the player to pick the part you want, then press Ctrl+E or the Export
button. That range is saved as a new clip in your library and the original
is left exactly as it was. I and O set the two points to whatever the
player is showing, so you can trim while you watch.

Trimming copies the picture and the sound as they were recorded rather than
making them again, so an export finishes in well under a second and loses
no quality at all.

The In handle moves about a second at a time, and the tick marks on the bar
show you why: a clip can only begin at one of those points, so Trix puts
your in point on the last one at or before where you dropped it. The Out
handle can land anywhere. Starting on an exact frame would mean making the
picture again, and that is not in yet.


NEW IN 0.6.0
------------

You can change where clips are saved without leaving the app:
Settings -> Trix -> Clips folder -> Choose... It opens the same picker the
tray icon has always had, so moving your library no longer means hunting
for the tray icon first. The box shows the folder your clips are really
going to, and Reset puts it back to %USERPROFILE%\Videos\Trix.

Clips you have already saved do not move. Changing the folder changes where
the NEXT clip is written; the ones you have stay where they are, and drop
out of the app's list until you point Trix back at them.

Trix also has a proper icon now, in place of the plain circle.


FIXED IN 0.5.3
--------------

The updater could not download anything. GitHub moved where release files
are served from, Trix did not recognise the new address, and updating
stopped with "updates are not fetched from
release-assets.githubusercontent.com". The "Download it by hand" link
offered underneath it did nothing when clicked, so there was no way forward
at all. Both are fixed, and links in the app now open in your browser.

PLEASE NOTE: this is a version you have to install by hand, once. Every
release up to and including 0.5.2 carries the broken updater inside it, so
none of them can fetch this one -- that is the bug. Download 0.8.0 from the
releases page and unzip it over your old folder. Your settings and your
clips live elsewhere and are not touched. From 0.5.3 onward the in-app
updater works normally again.


FIXED IN 0.5.2
--------------

"Show in Explorer" opened a brand new window every time you used it, so a
few clips in you had a stack of identical windows on the same folder. If
that folder is already open, Trix now brings that window forward -- from the
taskbar too, if you had minimised it -- and selects the new clip in it. A
window showing some other folder is left alone.


FIXED IN 0.5.1
--------------

0.4.0 and 0.5.0 closed themselves a few seconds after you pressed Start, and
straight away on the next launch, with no error message. The cause was the
update check: it asked github.com over an encrypted connection using a
component that was never included in the build, and that ended the app on the
spot. It is included now.

If you are on 0.4.0 or 0.5.0, Trix cannot update itself out of this, because
the update check is the thing that closed it. Download 0.8.0 by hand from the
releases page and unzip it over your old folder. Your settings and your clips
are somewhere else and are not touched.


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
