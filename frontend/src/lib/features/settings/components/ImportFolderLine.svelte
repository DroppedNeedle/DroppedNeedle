<!--
	One line under the Downloads page's drop zone, for admins: where the import
	folder is and what it is doing, or a pointer to set one.
-->
<script lang="ts">
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import {
		getImportFolderSettingsQuery,
		getImportFolderStatusQuery
	} from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { IMPORT_FOLDER_ANCHOR, describeImportFolder } from './importFolderStatus';

	const settingsQuery = getImportFolderSettingsQuery(() => authStore.isAdmin);
	const statusQuery = getImportFolderStatusQuery(() => authStore.isAdmin);

	const href = withBasePath(`/settings?tab=library#${IMPORT_FOLDER_ANCHOR}`);
	const path = $derived(settingsQuery.data?.path.trim() ?? '');
	const status = $derived(statusQuery.data ? describeImportFolder(statusQuery.data) : null);
</script>

{#if authStore.isAdmin && settingsQuery.data}
	<p class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted">
		{#if path === ''}
			<span>Bringing in a whole library?</span>
			<a class="link font-semibold text-base-content" {href}>Set an import folder</a>
		{:else}
			<span class="font-semibold text-base-content">Import folder</span>
			<span class="min-w-0 font-mono text-xs break-all">{path}</span>
			{#if status}
				<StatusChip size="xs" tone={status.tone}>{status.label}</StatusChip>
			{/if}
			<a class="link" {href}>Settings</a>
		{/if}
	</p>
{/if}
