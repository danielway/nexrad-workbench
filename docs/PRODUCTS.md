# Radar products

What each product in the product picker shows, and the few places where the
app does something you might not expect. Pick a product from the row under
**View** in the side panel, from the product menu in the top bar, or press `P`
to cycle through them.

| Code | Product | Unit |
|------|---------|------|
| REF | Reflectivity | dBZ |
| VEL | Velocity | m/s |
| SRV | Storm-relative velocity | m/s |
| SW | Spectrum width | m/s |
| ZDR | Differential reflectivity | dB |
| CC | Correlation coefficient | none |
| KDP | Specific differential phase | °/km |

## Reflectivity (REF)

How much energy came back from each gate, in dBZ. Higher values mean heavier
precipitation or hail. The lowest tilt reaches 460 km from the radar; the other
products stop at 300 km. Storm cells are always detected on reflectivity,
whatever product is on screen.

## Velocity (VEL)

The wind speed toward or away from the radar along each beam, in m/s. Values
away from the radar are positive and drawn in red; values toward it are
negative and drawn in green. Only the part of the wind along the
beam is measured, so wind blowing across the beam reads near zero.

Gates the radar marks as range folded are left blank. Velocity is not
dealiased: where the wind is faster than the radar's Nyquist velocity, colors
wrap to the other end of the scale.

On the lowest tilts the radar sweeps each angle twice, once for reflectivity
and once for Doppler. Velocity comes from the Doppler sweep.

## Storm-relative velocity (SRV)

Velocity with the storms' own motion taken out. Each gate shows
`velocity - (storm motion along that beam)`, so what remains is the wind
relative to the storm. Rotation in a moving storm, which plain velocity can
hide under the storm's motion, stands out as a tight pair of green and red
side by side.

**Where the storm motion comes from.** When you pick SRV, the app takes the
storm motion from the storm cell tracker: the median motion of the tracked
storms on screen, weighted by how confident each track is. Storm cells do
not need to be turned on; the app detects and tracks them in the background
for this, and draws nothing of them unless their layer is on.

**The motion is locked.** The motion is taken once, when you pick SRV, and
then held. It does not change from volume to volume, so the colors stay
steady while a loop plays. The side panel shows the motion in use, for
example "Storm motion NE 47 km/h, locked". Press **Update** next to it to take
the motion again from the storms on screen, for instance after jumping to a
different storm or day.

**Switching away and back takes a new motion.** The lock lasts while SRV is
the product on screen. If you switch to another product, such as VEL, and then
back to SRV, the app takes the motion again from the storms on screen at that
moment, so it may differ from the one you had before.

**Before there is a motion.** Tracks need at least two volumes, and a
confident motion usually needs a few more, so the app looks at the volumes
before the one on screen that are already downloaded. Until it has a motion,
SRV draws plain velocity and the panel says "No tracked storm motion yet, so
this is plain velocity". This also happens in clear air, or on a single
volume with nothing downloaded before it. Loop a stretch of the storm and the
motion appears once the tracks have one.

**Sharing.** A link to an SRV view carries the locked motion as
`srm=<direction>,<speed>` (the direction the storms move toward in degrees,
and km/h), so it opens with the same colors. The probe readout shows
storm-relative values, and SRV works in Rays playback and in 3D, including
volume rendering.

Like velocity, SRV is not dealiased.

## Spectrum width (SW)

How spread out the velocities are within each gate, in m/s. High values mean
turbulence or strong shear inside the beam.

## Differential reflectivity (ZDR)

The difference between the horizontal and vertical reflectivity, in dB. Large
raindrops are flattened as they fall and read positive; hail and tumbling
ice read near zero.

## Correlation coefficient (CC)

How alike the horizontal and vertical returns are, from 0 to about 1. Rain and
snow are near 1. Hail, mixed precipitation and especially debris from a
tornado read lower.

## Specific differential phase (KDP)

How fast the phase difference between the horizontal and vertical pulses
grows with range, in °/km. It rises with the amount of liquid water and is
not affected by hail, so it is a good guide to heavy rain.
