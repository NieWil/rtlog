if(CONFIG_RTLOG)
  zephyr_include_directories(${CMAKE_CURRENT_LIST_DIR}/../include)
  zephyr_compile_definitions(
    RTLOG_MAX_THREADS=${CONFIG_RTLOG_MAX_THREADS}
    RTLOG_RING_SIZE=${CONFIG_RTLOG_RING_SIZE}
  )
  if(CONFIG_RTLOG_STRIP)
    zephyr_compile_definitions(RTLOG_STRIP)
  endif()
  zephyr_library()
  zephyr_library_sources(rtlog_zephyr.c)
  # Keep the call-site descriptors in ROM with __start/__stop markers, or
  # (CONFIG_RTLOG_STRIP) in the ELF file only.
  zephyr_linker_sources(ROM_SECTIONS rtlog_sites.ld)
endif()
