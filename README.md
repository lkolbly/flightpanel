DIY Flightpanel
===============

Welcome to my DIY flightpanel. There are three components to this project:

* Mechanical design
* PCB design
* Firmware

All three are custom-built for this project.

PCB
===

The PCB has support for up to the following inputs:

* 16x push buttons
* 14x double-throw switches
* 6x rotary encoders with push-buttons
* 10x potentiometers or hall-effect sensors

(note that Windows only supports up to 8 axes)

Mechanical
==========

The mechanical design is in FreeCAD, in the `flightpanel.FCStd` file. Within
that project look at the Assembly for a reference design with all included
components, including stand-ins for various knobs and switches.

The design is oriented around providing a standardized base, with swappable
control scheme panels. Each panel has a working area of 60mm wide by 100mm tall
- see BigPanelTemplate in the FreeCAD project for a template. Two panels may be
combined to create a "wide" 128mm by 100mm panel.

The panel should extend 3.5mm past the centers of the holes for optimum
mechanical support.

The core structure provides space for up to 6 panels - two on the front face,
two on the top face (with an approximately 30 degree slope), and one on each
side.

The core consists of the following parts:
- `StructureFront`
- `StructureTop`
- `StructureBase`
- `SidePanel`

The mounting on the back is two M6 holes spaced 40mm apart vertically. Mouse bites are provided for additional rigidity. The `Mount` part provides a reference design for a mount - as modelled, for mounting to a 35mm wide square tube.

Hardware required for assembly:
- Heat-set M3 threaded inserts, designed for a 4.5mm diameter hole with 5mm depth.
- 14 to 38x M3 by 8mm bolts (14 for structure assembly, up to 24 for mounting panels).
- 2x M6 by 20mm bolts with nuts.
