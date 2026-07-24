return {
  -- snacks' fallback image renderer mispositions placements by one row when a tabline is visible, unless the
  -- snacks_image style is editor-relative. See snacks/image/placement.lua render_fallback.
  opts = {
    snacks_image = {
      relative = "editor",
    },
  },
}
