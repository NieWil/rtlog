/* rtlog call-site descriptors (read on the host by `rtlog contract --elf`).
 * The log path only uses their addresses. */
#if defined(CONFIG_RTLOG_STRIP) && defined(DEVNULL_REGION)
/* Not loaded: the descriptors and their strings are in zephyr.elf only. */
SECTION_PROLOGUE(rtlog_sites,(COPY),)
{
	__start_rtlog_sites = .;
	KEEP(*(rtlog_sites));
	__stop_rtlog_sites = .;
} GROUP_ROM_LINK_IN(DEVNULL_REGION, DEVNULL_REGION)
SECTION_PROLOGUE(rtlog_strs,(COPY),)
{
	KEEP(*(rtlog_strs));
} GROUP_ROM_LINK_IN(DEVNULL_REGION, DEVNULL_REGION)
#else
SECTION_PROLOGUE(rtlog_sites,,)
{
	__start_rtlog_sites = .;
	KEEP(*(rtlog_sites));
	__stop_rtlog_sites = .;
} GROUP_ROM_LINK_IN(RAMABLE_REGION, ROMABLE_REGION)
#endif
