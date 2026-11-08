-- Tint the window by the last command's exit status: pink after success,
-- red after a failure. Needs shell integration (OSC 133) for exit codes.

visual.add_layer({ id = "exit-tint", kind = "solid", color = "#f5a6d0", opacity = 0.18 })

visual.bind("exit-tint.color", {
  source = "terminal.last_exit_code",
  mapping = {
    [0] = "#f5a6d0",
    ["default"] = "#ff5577",
  },
  smoothing = 400,
})
