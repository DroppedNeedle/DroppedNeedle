<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Props {
		icon: KitIcon;
		title: string;
		/** `success` for "all done", `muted` for "nothing yet". */
		tone: 'success' | 'muted';
		children: Snippet;
		action?: Snippet;
	}

	let { icon: Icon, title, tone, children, action }: Props = $props();

	const tones = {
		success: { ring: 'bg-success/5', icon: 'h-8 w-8 text-success/30' },
		muted: { ring: 'bg-base-content/3', icon: 'h-8 w-8 text-base-content/15' }
	};
</script>

<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
	<div class="w-16 h-16 rounded-full {tones[tone].ring} flex items-center justify-center mb-4">
		<Icon class={tones[tone].icon} />
	</div>
	<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">{title}</h2>
	<p class="text-base-content/30 text-sm max-w-xs">{@render children()}</p>
	{@render action?.()}
</div>
