<script lang="ts">
	import type { Snippet } from 'svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Zone {
		id: string;
		label: string;
	}

	interface Props {
		zones: Zone[];
		// right-aligned action rendered in the same bar (e.g. the Customise button)
		action?: Snippet;
	}

	let { zones, action }: Props = $props();

	function jump(id: string) {
		const el = document.getElementById(id);
		if (!el) return;
		const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		el.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' });
	}
</script>

{#if zones.length > 1 || action}
	<nav
		class="sticky top-16 z-30 -mx-4 mb-2 border-b border-base-content/5 bg-base-100/85 px-4 py-2 backdrop-blur-md sm:-mx-6 sm:px-6 lg:-mx-8 lg:px-8"
		aria-label="Jump to a discover section"
	>
		<div class="flex items-center gap-2">
			<div class="flex flex-1 gap-1.5 overflow-x-auto">
				{#each zones as zone (zone.id)}
					<Button
						variant="ghost"
						size="xs"
						pill
						class="shrink-0 whitespace-nowrap"
						onclick={() => jump(zone.id)}
					>
						{zone.label}
					</Button>
				{/each}
			</div>
			{#if action}
				<div class="shrink-0">{@render action()}</div>
			{/if}
		</div>
	</nav>
{/if}
