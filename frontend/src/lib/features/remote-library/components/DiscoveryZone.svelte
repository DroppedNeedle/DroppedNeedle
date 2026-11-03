<script lang="ts">
	import { Sparkles } from 'lucide-svelte';
	import { reveal } from '$lib/actions/reveal';
	import type { Snippet } from 'svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		children: Snippet;
	}

	let { children }: Props = $props();
</script>

<div use:reveal class="relative mt-8 mb-4">
	<Card variant="bordered" class="relative overflow-hidden">
		<div
			class="pointer-events-none absolute inset-x-6 top-0 h-px bg-gradient-to-r from-transparent via-primary/30 to-transparent"
		></div>

		<div class="space-y-5">
			<div class="flex items-center gap-2.5">
				<div class="flex items-center justify-center rounded-lg bg-primary/10 p-1.5">
					<Sparkles class="h-4.5 w-4.5 text-primary" />
				</div>
				<h2 class="text-lg font-semibold tracking-tight text-base-content sm:text-xl">Discover</h2>
			</div>

			{@render children()}
		</div>
	</Card>
</div>
