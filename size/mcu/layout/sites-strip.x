
/* rtlog with the `strip` feature: descriptors and their strings stay in the
 * ELF file for `rtlog contract --elf`, but (INFO) sections are not loaded, so
 * they take no flash and are absent from the image that is flashed. */
SECTIONS
{
  linkme_SITES (INFO) : { KEEP(*(linkme_SITES)) }
  linkm2_SITES (INFO) : { KEEP(*(linkm2_SITES)) }
  rtlog_strs (INFO) : { KEEP(*(rtlog_strs)) }
}
