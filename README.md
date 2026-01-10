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
control scheme panels. Each panel has a working area of 60mm wide by 100mm tall - see BigPanelTemplate in the FreeCAD project for a template. Two panels may be
combined to create a "wide" 128mm by 100mm panel.

The core structure provides space for up to 6 panels - two on the front face,
two on the top face (with an approximately 30 degree slope), and one on each
side.
