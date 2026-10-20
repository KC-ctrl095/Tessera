// Mosaic avatars for bots: portraits cut from tesserae, plus emoji/text on a tile field.
//
//   <MosaicAvatar avatar={bot.avatar} size={32} color={bot.color} />
//   <AvatarPicker value={avatar()} onChange={setAvatar} color={bot.color} />
//
// Stored values: `m:<id>` for a portrait, anything else is an emoji or up to 4 characters.
//
// The app shell (sidebar, chats) imports MosaicAvatar.tsx and catalog.ts directly, so the
// picker stays out of its first download and comes with the bot editor.
export { MosaicAvatar, type MosaicAvatarProps } from "./MosaicAvatar";
export { AvatarPicker, type AvatarPickerProps } from "./AvatarPicker";
export { ALL_AVATARS, CATEGORY_LABEL, avatarGlyph, isKnownAvatar, isMosaicAvatar, mosaicId, randomAvatar, resolveAvatar, type AvatarCategory, type AvatarInfo } from "./catalog";
export { avatarDataUrl, preloadAvatars } from "./render";
