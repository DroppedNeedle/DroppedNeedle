<!--
	"Found on Discover, week 47": one line in an album's header when the
	caller kept it from one of their weekly sets, linking to that week. It
	reads the caller's own kept picks, kept in the query cache, and asks the
	server nothing for the album itself.
-->
<script lang="ts">
	import { Compass } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getFoundOnDiscoverQuery } from '$lib/features/discover/queries/weeks.svelte';

	interface Props {
		/** The album's release-group id. */
		mbid: string | null | undefined;
	}

	let { mbid }: Props = $props();

	const found = getFoundOnDiscoverQuery(() => Boolean(mbid));
	const pick = $derived.by(() => {
		const group = mbid?.toLowerCase();
		if (!group) return null;
		return found.data?.items.find((item) => item.release_group_mbid === group) ?? null;
	});
</script>

{#if pick}
	<Button
		variant="caps-link"
		size="sm"
		icon={Compass}
		href={withBasePath(`/discover/weeks?week=${pick.week_start_at}`)}
	>
		Found on Discover, week {pick.week_number}
	</Button>
{/if}
