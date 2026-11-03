<script lang="ts">
	import { page } from '$app/state';
	import LibraryManagementOperationPage from '$lib/features/library-management/components/LibraryManagementOperationPage.svelte';

	const jobId = $derived(page.params.id ?? '');
</script>

<LibraryManagementOperationPage {jobId} />
