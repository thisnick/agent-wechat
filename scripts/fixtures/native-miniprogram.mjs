// Independently authored legacy page fixture. No third-party package bytes.
import { createHash } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

const APP = "wx0123456789abcdef";
const EMBEDDED_IMAGE = "data:image/svg+xml," + encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><path fill="#12503e" d="M0 0h2v1H0z"/></svg>',
);

function page(route, id, initial, alternate, componentPage = false) {
  return `$gwx_${id}=function(_){return function(route){return function(data){
    if (route !== "./${route}.wxml") return;
    return {tag:"wx-page",children:[
      {tag:"wx-text",children:[data.title]},
      {tag:"wx-image",attr:{id:"embedded-image",src:data.image,style:"width:80rpx;height:40rpx",bindload:"imageLoaded"}},
      {tag:"wx-view",attr:{id:"embedded-background"}},
      {tag:"wx-input",attr:{placeholder:"Add a note",bindinput:"input",value:data.note}},
      {tag:"wx-text",children:[data.note]},
      {tag:"wx-button",attr:{bindtap:"toggle"},children:["Change title"]},
      {tag:"wx-view",attr:{id:"next-page",bindtap:"navigate","data-navigation":[["next",true]]},children:["Open next page"]},
      ...(data.privacyTest ? [{tag:"wx-button",attr:{openType:"agreePrivacyAuthorization",bindagreeprivacyauthorization:"acceptedPrivacy"},children:["Accept privacy terms"]}] : []),
      {tag:"wx-demo-counter",attr:{id:"counter",label:data.title,bindupdated:"countChanged"},
        children:[{tag:"wx-text",children:["Count reported: " + data.reported]}]}
    ]};
  }}}(0,__g.aa);
  __wxAppCode__['${route}.json'] = {"usingComponents":{"demo-counter":"../../components/counter/index"}};
  define("${route}.js",function(require,module,exports){
    const greeting = require("../../util");
    const definition = {
      data:{title:${JSON.stringify(initial)},image:${JSON.stringify(EMBEDDED_IMAGE)},note:"",changed:false,reported:0,
        local:{ read() { return true; } }},
      onLoad(query){
        if (!this.data.local.read()) throw new Error("Local data callback is missing");
        if (${componentPage} && !this.viewModel) throw new Error("Component page loaded before attachment");
        wx.setStorageSync("loadingRoute", getCurrentPages().at(-1)?.route);
        if(query.menu) this.setData({title:greeting(query.menu)});
        if(query.privacyTest) this.setData({privacyTest:true});
      },
      acceptedPrivacy(event){wx.setStorageSync("privacyEvent",event.detail.errMsg);},
      loadUtilities(){return Promise.all([require.async("/utilities/value"),require.async("/utilities/value.js")]);},
      toggle(){this.setData({title:this.data.changed?${JSON.stringify(initial)}:"Menu updated",changed:!this.data.changed});},
      input(event){this.setData({note:event.detail.value});},
      countChanged(event){this.setData({reported:event.detail.count});},
      imageLoaded(event){this.loadedImage=event.detail;},
      navigate(event){
        if (event.currentTarget.dataset.navigation?.[0]?.[1] !== true) throw new Error("Tap dataset lost its type");
        return wx.navigateTo({url:"/${alternate}?menu=next"});
      }
    };
    ${componentPage ? `const { data, ...methods } = definition;
    Component({
      data,
      methods,
      lifetimes:{ attached(){ this.viewModel = {}; } },
    });` : "Page(definition);"}
  },{isPage:true,isComponent:true,currentFile:'${route}.js'});
  `;
}

function archive(files) {
  const entries = Object.entries(files).map(([path, data]) => [Buffer.from(path), Buffer.from(data)]);
  const indexSize = entries.reduce((size, [path]) => size + 12 + path.length, 4);
  const payload = Buffer.concat(entries.map(([, content]) => content));
  const header = Buffer.alloc(14);
  header[0] = 0xbe;
  header.writeUInt32BE(indexSize, 5);
  header.writeUInt32BE(payload.length, 9);
  header[13] = 0xed;
  const index = Buffer.alloc(indexSize);
  index.writeUInt32BE(entries.length);
  let at = 4;
  let offset = 14 + indexSize;
  for (const [path, content] of entries) {
    index.writeUInt32BE(path.length, at);
    path.copy(index, at + 4);
    at += path.length + 4;
    index.writeUInt32BE(offset, at);
    index.writeUInt32BE(content.length, at + 4);
    at += 8;
    offset += content.length;
  }
  return Buffer.concat([header, index, payload]);
}

/** Stage a small two-page program for the disposable simulator account. */
export async function stageMiniProgram(dataDir, account, title = "Cafe") {
  const directory = join(dataDir, "hub", "miniprograms", account, APP);
  const home = "pages/home/index";
  const menu = "pages/menu/index";
  const files = {
    "icons/menu@2x.SVG": '<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><path fill="#12503e" d="M0 0h2v1H0z"/></svg>',
    "icons/menu.PNG": Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l2sAAAAASUVORK5CYII=", "base64"),
    "utilities/value.wasm": Buffer.from("0061736d010000000105016000017f030201000709010576616c756500000a06010400412a0b", "hex"),
    "app-config.json": JSON.stringify({ pages: [home, menu], subPackages: [{ root: "utilities" }] }),
    [home + ".html"]: `$gwx_Home('./${home}.wxml')`,
    [menu + ".html"]: `$gwx_Menu('./${menu}.wxml')`,
    "app-wxss.js": `
      setCssToHead([${JSON.stringify("#embedded-background{width:20rpx;height:10rpx;background-image:url(\"" + EMBEDDED_IMAGE + "\")}")}])();
      __COMMON_STYLESHEETS__['./shared.wxss'] = ["wx-text{color:rgb(18,80,62)}"];
      __wxAppCode__['pages/home/index.wxss'] = setCssToHead([[2,"./shared.wxss"],"body{padding:",[0,30],"}.mini-toolbar{display:none}wx-input{border:2px solid rgb(18,80,62)}"]);
      __wxAppCode__['pages/menu/index.wxss'] = setCssToHead([[2,"./shared.wxss"],"body{padding:",[0,30],"}.mini-toolbar{display:none}wx-input{border:2px solid rgb(18,80,62)}"]);
      __wxAppCode__['components/counter/index.wxss'] = setCssToHead(["wx-text{color:rgb(160,40,40)}"]);
    `,
    "app-service.js": `
      if(this&&this.__g===undefined)Object.defineProperty(this,"__g",{value:{aa:0}});
      Object.freeze(__g);
      $gwx_Counter = function(_) { return function(route) {
        if (route !== "./components/counter/index.wxml") return;
        return function(data) { return {tag:"wx-view",children:[
          {tag:"wx-text",children:[data.label + ": " + data.count]},
          {tag:"wx-button",attr:{bindtap:"increment"},children:["Add one"]},
          {tag:"wx-slot"}
        ]}; };
      }; }(0,__g.aa);
      if (__vd_version_info__.delayedGwx) {
        __wxAppCode__["components/counter/index.wxml"] = [$gwx_Counter, "./components/counter/index.wxml"];
      } else {
        __wxAppCode__["components/counter/index.wxml"] = $gwx_Counter("./components/counter/index.wxml");
      }
      define("util.js",function(require,module,exports,window,document,frames,self){
        const root = self?.Object === Object ? self : Function("return this")();
        if (root.Date !== Date || window !== undefined || document !== undefined || "fetch" in root) {
          throw new Error("Unexpected service global");
        }
        if (!wx.getSystemInfoSync().SDKVersion.split(".").every(part => Number.isInteger(Number(part)))) {
          throw new Error("Missing SDK version");
        }
        if (typeof print === "function") console.error = print;
        module.exports = function(menu){return "Menu: " + menu;};
      },{isPage:false,isComponent:false,currentFile:'util.js'});
      define("app.js",function(require,module,exports){
        App({
          onLaunch() {
            if (wx.cloud) wx.cloud.init({ env: "fixture", traceUser: true });
            wx.onAppShow(options => wx.setStorageSync("shownRoute", options.path));
            wx.onAppHide(() => wx.setStorageSync("hidden", true));
          }
        });
      },{isPage:false,isComponent:false,currentFile:'app.js'});
      define("components/counter/behavior.js",function(require,module,exports){
        module.exports = Behavior({
          data:{count:0},
          methods:{
            increment(){
              this.setData({count:this.data.count+1});
              this.triggerEvent("updated",{count:this.data.count});
            }
          }
        });
      },{isPage:false,isComponent:false,currentFile:'components/counter/behavior.js'});
      define("components/counter/index.js",function(require,module,exports){
        Component({
          lifetimes:{created(){this.createdWithRoute = this.is === "components/counter/index";}},
          properties:{label:{type:String,observer(){
            if(!this.createdWithRoute) throw new Error("Component properties observed before creation");
          }}},
          behaviors:[require("./behavior")]
        });
      },{isPage:false,isComponent:true,currentFile:'components/counter/index.js'});
      ${page(home, "Home", "Cafe home", menu)}
      ${page(menu, "Menu", "Cafe menu", home, true)}
    `,
  };
  // The browser view contains full templates. The service can retain a reduced
  // tree used by its own data tracking, which must not replace the view tree.
  files["app-wxss.js"] += files["app-service.js"];
  files["app-service.js"] = files["app-service.js"].replaceAll(
    '{tag:"wx-text",children:[data.title]}', '{tag:"wx-text",children:[]}',
  );
  const bytes = archive(files);
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, "manifest.json"), JSON.stringify({
    title, version: "1", packageId: "main",
    sha256: createHash("sha256").update(bytes).digest("hex"),
    entryPage: home, servicePath: "app-service.js",
  }));
  await writeFile(join(directory, "package.wxapkg"), bytes);
  const utilities = join(directory, "subpackages", "utilities");
  const utilityBytes = archive({
    "utilities/app-service.js": `define("utilities/value.js",function(require,module,exports){
      if (typeof print === "function") console.error = print;
      module.exports = {value: 42};
    },{isPage:false,isComponent:false,currentFile:'utilities/value.js'});`,
  });
  await mkdir(utilities, { recursive: true });
  await writeFile(join(utilities, "manifest.json"), JSON.stringify({
    root: "utilities", version: "1", sha256: createHash("sha256").update(utilityBytes).digest("hex"),
  }));
  await writeFile(join(utilities, "package.wxapkg"), utilityBytes);
  return APP;
}
