// Synthetic state/event comparison using the published Exparser-based engine.
const j=require('j-component');
globalThis.Behavior=j.behavior;globalThis.Component=j.register;
globalThis.wx={nextTick:callback=>queueMicrotask(callback)};
const computed=require('miniprogram-computed');
const mobx=require('mobx-miniprogram');
const bindings=require('mobx-miniprogram-bindings');
const lifecycle=[];j.register({id:'view',template:'<div><slot/></div>'});j.register({id:'button',template:'<div role="button"><slot/></div>'});
const id=j.register({template:'<view id="sum">{{sum}}</view><button id="increase" bindtap="increment">Increment</button>',behaviors:[computed.behavior],data:{a:1,b:2},computed:{sum:data=>data.a+data.b},methods:{increment(){this.setData({a:this.data.a+1});}},lifetimes:{created(){lifecycle.push('created')},attached(){lifecycle.push('attached')},detached(){lifecycle.push('detached')}}});
const component=j.create(id);component.attach(document.body);
globalThis.probe={component,lifecycle,async update(){component.setData({a:4});await Promise.resolve();return component.data.sum},async tap(){component.querySelector('#increase').dispatchEvent('tap');await new Promise(r=>setTimeout(r,0));return component.data.sum}};
const store=mobx.observable({value:2});
const m=j.register({template:'<view>{{value}}</view>',behaviors:[bindings.storeBindingsBehavior],storeBindings:{store,fields:['value']}});const mc=j.create(m);mc.attach(document.body);globalThis.probe.mobx=async()=>{store.value=7;await new Promise(r=>setTimeout(r,10));return mc.data.value};
