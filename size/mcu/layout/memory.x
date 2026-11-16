/* STM32F4-class part: 512 KiB flash, 128 KiB RAM. build.rs appends sites.x or
 * sites-strip.x, which place rtlog's site descriptors, and writes the result
 * to OUT_DIR/memory.x. */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 512K
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K
}
