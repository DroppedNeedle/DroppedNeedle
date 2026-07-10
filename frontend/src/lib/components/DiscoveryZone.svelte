<script lang="ts">
	import { Sparkles } from 'lucide-svelte';
	import { reveal } from '$lib/actions/reveal';
	import type { Snippet } from 'svelte';

	interface Props {
		children: Snippet;
	}

	let { children }: Props = $props();
</script>

<div use:reveal class="relative mt-8 mb-4">
	<div class="relative overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50">
		<div
			class="pointer-events-none absolute inset-x-6 top-0 h-px bg-gradient-to-r from-transparent via-accent/40 to-transparent"
		></div>

		<div class="space-y-6 p-5 sm:p-6">
			<h2
				class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
			>
				<Sparkles class="h-4 w-4 text-accent" />
				Discover
			</h2>

			{@render children()}
		</div>
	</div>
</div>
