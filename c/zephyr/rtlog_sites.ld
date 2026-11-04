/* rtlog call-site descriptors (read on the host by `rtlog contract --elf`).
 * The log path only uses their addresses. */
SECTION_PROLOGUE(rtlog_sites,,)
{
	__start_rtlog_sites = .;
	KEEP(*(rtlog_sites));
	__stop_rtlog_sites = .;
} GROUP_ROM_LINK_IN(RAMABLE_REGION, ROMABLE_REGION)
