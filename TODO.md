Do a general study on ways to improve rendering quality
In the timeline portion of the UI we have blue age text; make it a single time rather than a range

Consider always showing cursor coordinates and elevation, but off the side of the UI instead of over the cursor with the inspector only

Add a progress modal when we're downloading data to give a sense of total completion (downloads, decompress and decodes, etc)
There's often a garbage volume record (around the top of the hour) which may be the "MDM" one? We should consider just skipping those.

Miscellaneous improvement to 3D mode to bring into parity with 2D
Instead of a sweep line, we should show a ray in 3D for the gate azimuth+elevation

We have some very fine details (sweep animation, displaying sweeps individually in the timeline) and some desired macro details
    (showing frames matching the filter as equidistant); should we change the app to operate in two distinct regimes?
1. A micro regime where we have sweep- and radial-level details
2. A macro regime where we operate on frames only and faster playback
Does the user explicitly swap, or do we use timeline zoom level and make it really clear in the UI?
What other parts of our experience would change to reflect the mode they're in?

How is snow rendered properly?

Improve the radar operations view: combine azimuth and elevation, change BG color, narrow the side panel, add estimated or real timings to each elevation

Cell detection
Storm motion and tracking
Storm-relative motion

Allow linking to the app in a way that automatically triggers real-time streaming
Add blanket protection against infinite request loops
Add "are you still streaming" popup after some amount of time
Look into optimizing real-time streaming if the user has a fixed tilt (e.g. guessing which chunks will have that sweep)

Add layers for
    cities and towns, revealed based on zoom level and population size
    roads, interstates, highways, minor roads, revealed by zoom
and render all atop the radar

Performance considerations
    even at idle, app uses 70% CPU (can we save some render work? is any due to rendering every second? could we partition live from less-live parts of egui?)
        macro uses MUCH less cpu than micro, so likely related to timeline or render complexity?
    generally, study when we might be rendering more often than needed
    move layer rendering to separate thing that is invalidated less often

The year lines are wrong: when zooming between years and months, they are wrong

Avoid re-downloading chunks when restarting real-time that are already downloaded

Move layer rendering to cached and async thing, so it's not every frame

Improvements to realtime rendering
    When the live sweep line passes enough azimuth for a full chunk (1/3 or 1/6 depending on VCP), replace the
        render for that portion of the canvas with a flashing loading indicator with the "next chunk" count down
    Show "now" on the live sweep line
    Ensure the sweep numbers and parameters are actually correct for the sweep line (particularly
        that it isn't showing what's being received vs. where the line/radar actually is)
    Always show times on boundaries for chunks being rendered
    Instead of the "C1 - 120r" indication, show the sweep, then the chunk (1/3), the azimuth range, and
        finally the age range
    In addition to desaturating data within 1/4 ahead of the sweep line, also fully desaturate back to the latest chunk
    Ensure we consistently predict the number of chunks for a sweep based on params (3 or 6)
    Ensure the timeline clearly marks the expected chunks in time and in count (e.g. 0/6)
    Ensure we have a consistent model throughout the application for representing what is going on (e.g.
        what sweeps/chunks are in the past, which sweep is being received actively and which chunk is next,
        and finally what is actively happening at this moment at the radar site)
    Sometimes streaming gets stuck polling a chunk that never resolves (it should fail, but also why is it stuck?)
    Show elevation numbers in VCP panel

4/4

When we start real-time streaming, we should change the routine to only downloading the first chunk (with VCP metadata)
    and the latest sweep's chunks. We should not do the backfill anymore by default.

We need retry limits for both acquisition (in case the binary search logic fails) and for waiting for an expected chunk.
    It should stop real-time streaming and provide an error message.

At idle, the app eats a lot of CPU. How can we optimize it to reduce how expensive/heavy it is?
