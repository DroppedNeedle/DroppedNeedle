<script lang="ts">
	import {
		appearance,
		THEME_OPTIONS,
		TEXT_SCALE_OPTIONS,
		type ThemeChoice
	} from '$lib/stores/appearance.svelte';
	import { BOTTOM_NAV_ITEMS, bottomNavItem } from '$lib/shell/bottomNavItems';
	import { Check, Monitor, Moon, Sun, Contrast, Type, Zap, Smartphone } from 'lucide-svelte';

	const themeIcons: Record<ThemeChoice, typeof Monitor> = {
		system: Monitor,
		droppedneedle: Moon,
		'droppedneedle-light': Sun,
		'droppedneedle-contrast': Contrast
	};

	// the concrete theme a swatch should preview (System previews the OS resolution)
	function previewTheme(value: ThemeChoice): string {
		return value === 'system' ? appearance.resolvedTheme : value;
	}
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl mb-2">Appearance</h2>
		<p class="text-base-content/70 mb-6">
			Make DroppedNeedle yours. These settings live in this browser and apply instantly — nothing to
			save, and they never affect anyone else on your instance.
		</p>

		<!-- Theme -->
		<div class="mb-8">
			<h3 class="text-xl font-semibold mb-1">Theme</h3>
			<p class="text-base-content/60 mb-4 text-sm">
				Pick a look, or follow your device's light/dark setting automatically.
			</p>

			<div class="grid grid-cols-2 lg:grid-cols-4 gap-3">
				{#each THEME_OPTIONS as option (option.value)}
					{@const Icon = themeIcons[option.value]}
					{@const active = appearance.theme === option.value}
					<button
						type="button"
						onclick={() => appearance.setTheme(option.value)}
						aria-pressed={active}
						class="group relative flex flex-col gap-3 rounded-2xl border p-3 text-left transition-all duration-200 {active
							? 'border-primary/60 bg-primary/5 shadow-[var(--dn-shadow-2)]'
							: 'border-base-300/50 hover:border-base-content/25 hover:bg-base-300/30'}"
					>
						{#if active}
							<span
								class="absolute right-2 top-2 flex h-5 w-5 items-center justify-center rounded-full bg-primary text-primary-content"
							>
								<Check class="h-3 w-3" />
							</span>
						{/if}

						<!-- live swatch rendered in the target theme -->
						<div
							data-theme={previewTheme(option.value)}
							class="relative h-16 overflow-hidden rounded-xl border border-base-content/10 bg-base-100"
						>
							<div class="absolute inset-0 flex items-end gap-1.5 p-2.5">
								<span class="h-8 flex-1 rounded-md bg-base-300"></span>
								<span class="h-10 w-3 rounded-md bg-primary"></span>
								<span class="h-6 w-3 rounded-md bg-accent"></span>
							</div>
							<div
								class="absolute left-2.5 top-2.5 h-1.5 w-8 rounded-full bg-base-content/40"
							></div>
						</div>

						<div class="flex items-center gap-2">
							<Icon class="h-4 w-4 {active ? 'text-primary' : 'text-base-content/60'}" />
							<div class="min-w-0">
								<div class="truncate text-sm font-semibold leading-tight">{option.label}</div>
								<div class="truncate text-[11px] text-base-content/50">{option.hint}</div>
							</div>
						</div>
					</button>
				{/each}
			</div>
		</div>

		<!-- Text size -->
		<div class="mb-8">
			<h3 class="text-xl font-semibold mb-1 flex items-center gap-2">
				<Type class="h-5 w-5 text-base-content/50" /> Text size
			</h3>
			<p class="text-base-content/60 mb-4 text-sm">
				Scales the whole interface. Handy on a TV across the room or if you just prefer bigger type.
			</p>

			<div class="join">
				{#each TEXT_SCALE_OPTIONS as option (option.value)}
					{@const active = appearance.textScale === option.value}
					<button
						type="button"
						onclick={() => appearance.setTextScale(option.value)}
						aria-pressed={active}
						class="btn join-item {active ? 'btn-primary' : 'btn-ghost bg-base-300/40'}"
					>
						{option.label}
					</button>
				{/each}
			</div>
		</div>

		<!-- Mobile bottom bar -->
		<div class="mb-8">
			<h3 class="text-xl font-semibold mb-1 flex items-center gap-2">
				<Smartphone class="h-5 w-5 text-base-content/50" /> Mobile bottom bar
			</h3>
			<p class="text-base-content/60 mb-4 text-sm">
				Choose the four shortcuts on your phone's bottom bar. The fifth slot is always Menu. Picking
				a destination already in use swaps the two slots.
			</p>

			<div class="grid grid-cols-2 gap-3 sm:grid-cols-4">
				{#each appearance.bottomNav as slotId, i (i)}
					{@const current = bottomNavItem(slotId)}
					<label class="flex flex-col gap-1.5">
						<span
							class="font-mono text-[0.6rem] font-bold uppercase tracking-[0.2em] text-base-content/40"
							>Slot {i + 1}</span
						>
						<div class="flex items-center gap-2">
							<span
								class="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-base-content/6"
							>
								<current.icon class="h-4 w-4 opacity-70" />
							</span>
							<select
								class="select select-sm min-w-0 flex-1"
								value={slotId}
								onchange={(e) =>
									appearance.setBottomNavSlot(i, (e.currentTarget as HTMLSelectElement).value)}
								aria-label="Bottom bar slot {i + 1}"
							>
								{#each BOTTOM_NAV_ITEMS as item (item.id)}
									<option value={item.id}>{item.label}</option>
								{/each}
							</select>
						</div>
					</label>
				{/each}
			</div>

			<!-- live preview of the bar -->
			<div
				class="mt-4 flex max-w-sm items-center justify-between gap-1 rounded-2xl border border-base-content/8 bg-base-200/70 px-3 py-2.5"
				aria-hidden="true"
			>
				{#each appearance.bottomNav as slotId (slotId)}
					{@const item = bottomNavItem(slotId)}
					<span class="flex flex-col items-center gap-1 text-[0.6rem] text-base-content/60">
						<item.icon class="h-4 w-4" />
						{item.label}
					</span>
				{/each}
				<span class="flex flex-col items-center gap-1 text-[0.6rem] text-base-content/40">
					<Smartphone class="h-4 w-4" />
					Menu
				</span>
			</div>

			<button
				class="btn btn-ghost btn-sm mt-3 rounded-full"
				onclick={() => appearance.resetBottomNav()}
			>
				Reset to defaults
			</button>
		</div>

		<!-- Motion -->
		<div>
			<h3 class="text-xl font-semibold mb-1 flex items-center gap-2">
				<Zap class="h-5 w-5 text-base-content/50" /> Motion
			</h3>
			<p class="text-base-content/60 mb-4 text-sm">
				Reduce animations and transitions across the app. "System" respects your device's
				reduce-motion setting.
			</p>

			<label class="flex items-center gap-3 cursor-pointer w-fit">
				<input
					type="checkbox"
					class="toggle toggle-primary"
					checked={appearance.motion === 'reduced'}
					onchange={(e) =>
						appearance.setMotion(
							(e.currentTarget as HTMLInputElement).checked ? 'reduced' : 'system'
						)}
				/>
				<span class="text-sm">
					Reduce motion
					<span class="text-base-content/50">
						({appearance.motion === 'reduced' ? 'on' : 'following system'})
					</span>
				</span>
			</label>
		</div>
	</div>
</div>
