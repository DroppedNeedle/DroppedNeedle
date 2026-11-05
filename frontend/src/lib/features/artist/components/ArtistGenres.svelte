<!--
	The artist's genres as chips, each a link to its genre page: the
	MusicBrainz tags first, then Last.fm's that are new. The first few show;
	"+n" opens the rest in place.
-->
<script lang="ts">
	import Card from '$lib/ui/Card.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		tags: string[];
		/** Layout only. */
		class?: string;
	}

	let { tags, class: className = '' }: Props = $props();

	const SHOWN = 6;
	let open = $state(false);

	const unique = $derived(
		tags.filter(
			(tag, index) =>
				tag.trim() !== '' &&
				tags.findIndex((other) => other.toLowerCase() === tag.toLowerCase()) === index
		)
	);
	const shown = $derived(open ? unique : unique.slice(0, SHOWN));
	const hidden = $derived(unique.length - shown.length);
</script>

{#if unique.length > 0}
	<Card
		as="section"
		variant="bordered"
		padding="none"
		class={className}
		aria-labelledby="artist-genres-title"
	>
		<div class="p-5">
			<h2 id="artist-genres-title" class="dn-genres__title">Genres</h2>
			<ul class="mt-3 flex flex-wrap gap-2">
				{#each shown as tag (tag)}
					<li>
						<Button
							variant="outline"
							size="sm"
							pill
							href={withBasePath(`/genre?name=${encodeURIComponent(tag)}`)}>{tag}</Button
						>
					</li>
				{/each}
				{#if hidden > 0}
					<li>
						<Button
							variant="outline"
							size="sm"
							pill
							aria-expanded={false}
							aria-label="Show {hidden} more genres"
							onclick={() => (open = true)}>+{hidden}</Button
						>
					</li>
				{/if}
			</ul>
		</div>
	</Card>
{/if}

<style>
	.dn-genres__title {
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		color: var(--color-base-content);
	}
</style>
