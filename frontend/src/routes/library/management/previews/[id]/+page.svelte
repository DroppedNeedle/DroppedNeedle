<script lang="ts">
	import { page } from '$app/state';
	import LibraryManagementPreviewPage from '$lib/features/library-management/components/LibraryManagementPreviewPage.svelte';

	const jobId = $derived(page.params.id ?? '');
</script>

<LibraryManagementPreviewPage {jobId} />
