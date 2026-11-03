<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { ChevronLeft } from 'lucide-svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		fallback?: string;
	}

	let { fallback = '/' }: Props = $props();

	function goBack() {
		if (browser && window.history.length > 1) {
			window.history.back();
		} else {
			goto(withBasePath(fallback));
		}
	}
</script>

<IconButton variant="neutral" icon={ChevronLeft} label="Go back" onclick={goBack} />
