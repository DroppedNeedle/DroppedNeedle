<!--
	Opens the share image sheet for a year. `primary` is the hero's button,
	`outline` the one at the page's foot. A year with too few plays has no
	image, so it shows nothing.
-->
<script lang="ts">
	import { Image as ImageIcon } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import type { YearReview } from '../queries/year.svelte';
	import SaveImageSheet from './SaveImageSheet.svelte';

	interface Props {
		review: YearReview;
		look?: 'primary' | 'outline';
	}

	let { review, look = 'primary' }: Props = $props();

	let open = $state(false);
</script>

{#if review.state !== 'too_few_plays'}
	<Button
		variant={look}
		size={look === 'primary' ? 'display' : 'display-sm'}
		icon={look === 'primary' ? ImageIcon : undefined}
		onclick={() => (open = true)}
	>
		Save image
	</Button>
	<SaveImageSheet {review} bind:open />
{/if}
