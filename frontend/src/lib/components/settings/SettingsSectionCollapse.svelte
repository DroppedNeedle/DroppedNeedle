<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	type Props = {
		title: string;
		description: string;
		icon: KitIcon;
		iconBgClass?: string;
		iconTextClass?: string;
		isOpen?: boolean;
		name?: string;
		children: Snippet;
	};

	let {
		title,
		description,
		icon: Icon,
		iconBgClass = 'bg-primary/10',
		iconTextClass = 'text-primary',
		isOpen = $bindable(false),
		name = 'advanced-settings',
		children
	}: Props = $props();
</script>

<div class="collapse collapse-arrow bg-base-200 rounded-box">
	<input type="radio" {name} checked={isOpen} aria-label={title} onchange={() => (isOpen = true)} />
	<div class="collapse-title">
		<div class="flex items-center gap-3">
			<div class="{iconBgClass} p-2 rounded-lg">
				<Icon class="w-5 h-5 {iconTextClass}" />
			</div>
			<div>
				<h3 class="font-semibold text-base">{title}</h3>
				<p class="text-xs text-base-content/50">{description}</p>
			</div>
		</div>
	</div>
	<div class="collapse-content">
		{@render children()}
	</div>
</div>
