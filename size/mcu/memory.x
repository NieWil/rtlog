/* STM32F4-class part: 512 KiB flash, 128 KiB RAM. */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 512K
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K
}

/* rtlog site descriptors: keep them in flash with the other read-only data.
 * Without this, the linker places them as orphan sections after RAM data,
 * where nothing initialises them at reset. */
SECTIONS
{
  linkme_SITES : { KEEP(*(linkme_SITES)) } > FLASH
  linkm2_SITES : { KEEP(*(linkm2_SITES)) } > FLASH
} INSERT AFTER .rodata;
