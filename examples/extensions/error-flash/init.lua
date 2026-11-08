-- Flash the window red when output contains ERROR.
--
-- `terminal.output` carries the printed text (escape sequences removed);
-- `terminal.output.raw` would carry the bytes, and `command.output` only
-- what the running command prints.

local pattern = seraphim.settings.pattern or "ERROR"

-- A transparent red layer on top of the background, ours to animate.
visual.add_layer({ id = "error-flash", kind = "solid", color = "#ff2255", opacity = 0 })

animation.define("error_flash", {
  { layer = "error-flash", param = "opacity", from = 0.45, to = 0, duration = 900, easing = "ease-out" },
})

-- Output arrives in bursts; look at most every 100 ms.
seraphim.on("terminal.output", function(event)
  if event.text:find(pattern, 1, true) then
    animation.play("error_flash")
  end
end, { throttle = 100 })
