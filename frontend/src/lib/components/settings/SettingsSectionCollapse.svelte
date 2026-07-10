<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { SvelteComponent } from 'svelte';

	type Props = {
		title: string;
		description: string;
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		icon: typeof SvelteComponent<any>;
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

<div class="collapse collapse-arrow rounded-2xl border border-base-content/8 bg-base-200/50">
	<input type="radio" {name} checked={isOpen} onchange={() => (isOpen = true)} />
	<div class="collapse-title">
		<div class="flex items-center gap-3.5">
			<div class="{iconBgClass} grid size-10 shrink-0 place-items-center rounded-xl">
				<Icon class="w-5 h-5 {iconTextClass}" />
			</div>
			<div class="min-w-0">
				<h3
					class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					{title}
				</h3>
				<p class="mt-0.5 truncate text-sm text-base-content/70">{description}</p>
			</div>
		</div>
	</div>
	<div class="collapse-content">
		{@render children()}
	</div>
</div>
