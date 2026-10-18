# contrib

## hyprland.example.conf

Window rules for Hyprland 0.53 or newer (it uses the `match:` windowrule syntax). They put the editor (`retro-paint-editor`) tiled on the left monitor and the DOS view (`retro-paint-dos`) floating on the right monitor, filling it. On a vertical (9:16) monitor that gives the mobile layout at full size.

1. Copy the example to your own config. `contrib/hyprland.conf` is gitignored, so your monitor names stay out of the repo and pulls never overwrite it:

   ```
   cp contrib/hyprland.example.conf contrib/hyprland.conf
   ```

2. Run `hyprctl monitors` and set `$rp_left` and `$rp_right` at the top of `contrib/hyprland.conf` to your monitor names.
3. Source it from `~/.config/hypr/hyprland.conf`:

   ```
   source = /path/to/retro-paint/contrib/hyprland.conf
   ```

4. Check it with `hyprctl reload` (errors show in the Hyprland error bar) or `Hyprland --verify-config`.

For the desktop (16:9) DOS layout or a landscape right monitor, swap the `size`/`move` rules for the commented 16:9 example at the bottom of the file.

The rules also turn off opacity, blur, dimming, shadows, borders and rounding on the DOS window, so captured pixels stay exact CGA colors.

## OBS capture

Capture the `retro-paint-dos` window alone (a PipeWire window capture of the window with that app_id), not the whole screen. The DOS window never takes input, so it can sit on any monitor while you work in the editor.

## Single monitor

The editor and DOS view are always two separate OS windows. With one screen, drop the `monitor` rules (or point both variables at the same monitor), arrange the two windows yourself, and have OBS capture the DOS window by itself.
