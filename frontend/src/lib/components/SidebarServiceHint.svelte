<script lang="ts">
	import { resolve } from '$app/paths';
	import { Plus } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		icon: Snippet;
		label: string;
		settingsTab: string;
	}

	let { icon, label, settingsTab }: Props = $props();
</script>

<li class="opacity-30 hover:opacity-70 transition-opacity duration-200">
	<a
		href={resolve(`/settings?tab=${settingsTab}`)}
		class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
		data-tip="Connect {label}"
	>
		<div class="relative">
			{@render icon()}
			<span
				class="absolute -bottom-1 -right-1 bg-base-300 rounded-full p-0.5 flex items-center justify-center"
			>
				<Plus class="w-2.5 h-2.5" />
			</span>
		</div>
		<span class="is-drawer-close:hidden">{label}</span>
	</a>
</li>
